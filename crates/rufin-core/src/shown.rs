//! Committed changes shown in the rows a client read earlier: the one owner of shown-row
//! updates on every surface (GTK pages, Home, Search and headers, Android pages).

use std::future::Future;
use std::sync::{Arc, Mutex, PoisonError};

use library::{FolderKey, ReadCancellation, ShowsState};

use crate::source::SourceOwner;
use crate::source::feed::Changed;

/// The favorites, ratings and downloads committed while a surface reads rows. Rows read
/// before a change committed and shown after it went out would miss it, so a surface takes
/// one before it reads and shows what it [`missed`](Self::missed) once it shows the rows.
pub struct Reading(pub(crate) tokio::sync::broadcast::Receiver<Arc<Changed>>);

impl Reading {
    /// The favorites, ratings and downloads committed since it started; None when nothing was.
    /// A surface shows it in the rows it read as any other change.
    pub fn missed(mut self) -> Option<Changed> {
        use tokio::sync::broadcast::error::TryRecvError;
        let mut missed = Changed::default();
        loop {
            match self.0.try_recv() {
                Ok(changed) => {
                    missed.items.extend(changed.items.iter().cloned());
                    missed.downloads.extend(changed.downloads.iter().cloned());
                }
                Err(TryRecvError::Lagged(_)) => {}
                Err(TryRecvError::Empty | TryRecvError::Closed) => break,
            }
        }
        (!missed.items.is_empty() || !missed.downloads.is_empty()).then_some(missed)
    }
}

/// The download read a surface's newest change started.
#[derive(Default)]
struct Pending {
    generation: u64,
    read: Option<(ReadCancellation, tokio::task::AbortHandle)>,
}

/// The changes one surface shows in its rows. A newer change with downloads supersedes the
/// download read of an older one: that read is cancelled and nothing of it applies.
#[derive(Clone, Default)]
pub struct ShownChanges(Arc<Mutex<Pending>>);

/// What a change shows in a surface's rows.
pub struct Showing<F> {
    /// Whether favorites or ratings changed any of the rows.
    pub states: bool,
    /// The rows whose downloads changed, by their index among the rows passed, once the Catalog
    /// counted them; None when the change named no downloads. It resolves to None when a newer
    /// change superseded it.
    pub downloads: Option<F>,
}

impl ShownChanges {
    /// Shows `changed` in `rows`, the rows the surface shows now: favorites and ratings at once,
    /// and, when it names downloads, the downloads as the Catalog counts them, read in the
    /// background. Smart playlists count as of `smart`, [`crate::view::View::smart_count`]. A
    /// client puts each returned row in place of the one it shows at that index with
    /// [`ShowsState::take_downloads`], which keeps what changed since.
    pub fn show<T>(
        &self,
        owner: &SourceOwner,
        rows: &mut [T],
        changed: &Changed,
        smart: (Option<i64>, Option<FolderKey>),
    ) -> Showing<impl Future<Output = Option<Vec<(usize, T)>>> + Send + 'static + use<T>>
    where
        T: ShowsState + Clone + PartialEq + Send + 'static,
    {
        let states = rows
            .iter_mut()
            .fold(false, |shown, row| row.show_states(&changed.items) || shown);
        if changed.downloads.is_empty() || rows.is_empty() {
            return Showing {
                states,
                downloads: None,
            };
        }
        let shown = rows.to_vec();
        let catalog = Arc::clone(&owner.shared.catalog);
        let cancel = ReadCancellation::new();
        let reading = cancel.clone();
        let task = owner.shared.runtime.spawn(async move {
            let mut read = shown.clone();
            catalog
                .show_downloads(
                    read.iter_mut().filter_map(ShowsState::downloads).collect(),
                    smart,
                    &reading,
                )
                .await?;
            Ok::<_, library::LibraryError>(
                read.into_iter()
                    .zip(shown)
                    .enumerate()
                    .filter(|(_, (read, shown))| read != shown)
                    .map(|(index, (read, _))| (index, read))
                    .collect::<Vec<_>>(),
            )
        });
        let generation = {
            let mut pending = self.0.lock().unwrap_or_else(PoisonError::into_inner);
            if let Some((cancel, task)) = pending.read.take() {
                cancel.cancel();
                task.abort();
            }
            pending.generation += 1;
            pending.read = Some((cancel, task.abort_handle()));
            pending.generation
        };
        let pending = Arc::clone(&self.0);
        Showing {
            states,
            downloads: Some(async move {
                let read = match task.await {
                    Ok(Ok(read)) => read,
                    Ok(Err(error)) => {
                        tracing::warn!(%error, "could not show downloads");
                        return None;
                    }
                    Err(_) => return None,
                };
                let mut pending = pending.lock().unwrap_or_else(PoisonError::into_inner);
                (pending.generation == generation).then(|| {
                    pending.read = None;
                    read
                })
            }),
        }
    }
}
