//! Owners that follow the change feed.

use std::future::Future;
use std::sync::{Arc, Weak};

use library::{Catalog, Change};
use tokio::sync::broadcast::error::{RecvError, TryRecvError};

/// Runs `pass` once the Catalog loaded: a full pass (`None`) first and after the feed lagged,
/// then one per batch of Changes heard since the previous pass. It ends with its owner.
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
    let mut changes = catalog.changes();
    let catalog = catalog.clone();
    runtime
        .spawn(async move {
            catalog.loaded().await;
            drop(catalog);
            let (mut full, mut heard) = (true, Vec::new());
            loop {
                loop {
                    match changes.try_recv() {
                        Ok(change) => heard.push(change),
                        Err(TryRecvError::Lagged(_)) => full = true,
                        Err(TryRecvError::Empty) => break,
                        Err(TryRecvError::Closed) => return,
                    }
                }
                if full || !heard.is_empty() {
                    let Some(owner) = owner.upgrade() else {
                        return;
                    };
                    let heard = std::mem::take(&mut heard);
                    pass(owner, (!std::mem::take(&mut full)).then_some(heard)).await;
                    continue;
                }
                match changes.recv().await {
                    Ok(change) => heard.push(change),
                    Err(RecvError::Lagged(_)) => full = true,
                    Err(RecvError::Closed) => return,
                }
            }
        })
        .abort_handle()
}
