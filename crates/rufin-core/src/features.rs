//! Owners that follow the change feed.

use std::future::Future;
use std::sync::{Arc, Weak};

use library::{Catalog, Change};

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
