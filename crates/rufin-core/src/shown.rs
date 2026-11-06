//! Committed changes shown in the rows a client read earlier: the one owner of shown-row
//! updates on every surface (GTK pages, Home, Search and headers, Android pages).

use std::future::Future;
use std::sync::{Arc, Mutex, PoisonError};

use library::{ReadCancellation, ShowsState};

use crate::source::SourceOwner;
use crate::source::feed::Changed;

/// What committed while a surface reads rows. Rows read before a change committed and shown
/// after it went out would miss it, so a surface takes one before it reads and, once it shows
/// the rows, handles what it [`missed`](Self::missed) as any other change: shows it in the rows
/// and asks its page's [`crate::view::View::refresh`] what it means.
pub struct Reading(pub(crate) tokio::sync::broadcast::Receiver<Arc<Changed>>);

impl Reading {
    /// Everything committed since it started, everything when it fell behind; None when
    /// nothing was.
    pub fn missed(mut self) -> Option<Changed> {
        use tokio::sync::broadcast::error::TryRecvError;
        let mut missed = Changed::default();
        loop {
            match self.0.try_recv() {
                Ok(changed) => missed.extend(&changed),
                Err(TryRecvError::Lagged(_)) => missed.all = true,
                Err(TryRecvError::Empty | TryRecvError::Closed) => break,
            }
        }
        (missed != Changed::default()).then_some(missed)
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
pub struct Showing<T, F> {
    /// The rows whose favorites or ratings changed, by their index among the rows passed,
    /// showing them.
    pub states: Vec<(usize, T)>,
    /// The rows whose downloads changed, by their index among the rows passed, once the Catalog
    /// counted them; None when the change named no downloads. It resolves to None when a newer
    /// change superseded it.
    pub downloads: Option<F>,
}

impl ShownChanges {
    /// Shows `changed` in `rows`, the rows the surface shows now, read by `view` (None for rows
    /// read with no page, in no music folder): favorites and ratings at once, and, when it names
    /// downloads, the downloads as the Catalog counts them in the music folder `view` reads,
    /// read in the background. Only the rows that change come back. A client puts each in place of the row it shows at that
    /// index, a downloads row with [`ShowsState::take_downloads`], which keeps what changed
    /// since.
    pub fn show<T>(
        &self,
        owner: &SourceOwner,
        rows: &[&T],
        changed: &Changed,
        view: Option<&crate::view::View>,
    ) -> Showing<T, impl Future<Output = Option<Vec<(usize, T)>>> + Send + 'static + use<T>>
    where
        T: ShowsState + Clone + Send + 'static,
    {
        let states = rows
            .iter()
            .enumerate()
            .filter(|(_, row)| {
                changed.items.iter().any(|change| {
                    row.state(change)
                        .is_some_and(|state| !change.shown_in(state))
                })
            })
            .map(|(index, row)| {
                let mut row = T::clone(row);
                row.show_states(&changed.items);
                (index, row)
            })
            .collect();
        if changed.downloads.is_empty() || rows.is_empty() {
            return Showing {
                states,
                downloads: None,
            };
        }
        let counted = view.map(crate::view::View::counted).unwrap_or_default();
        let mut read = rows.iter().map(|row| T::clone(row)).collect::<Vec<_>>();
        let catalog = Arc::clone(&owner.shared.catalog);
        let cancel = ReadCancellation::new();
        let reading = cancel.clone();
        let task = owner.shared.runtime.spawn(async move {
            let shown = read
                .iter_mut()
                .map(|row| row.downloads().map(|row| row.downloaded()))
                .collect::<Vec<_>>();
            catalog
                .show_downloads(
                    read.iter_mut().filter_map(ShowsState::downloads).collect(),
                    counted,
                    &reading,
                )
                .await?;
            Ok::<_, library::LibraryError>(
                read.into_iter()
                    .zip(shown)
                    .enumerate()
                    .filter_map(|(index, (mut row, shown))| {
                        (row.downloads().map(|row| row.downloaded()) != shown)
                            .then_some((index, row))
                    })
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
