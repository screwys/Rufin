use std::sync::{Arc, Mutex, Weak};

use tokio::sync::watch;

use crate::{PlaybackProjection, PlaybackView};

/// Shared playback observations. State ticks coalesce; each subscriber retains
/// its unread notices until it consumes them.
#[derive(Clone, Default)]
pub struct PlaybackUpdates(Arc<Mutex<Updates>>);

#[derive(Default)]
struct Updates {
    current: Option<Arc<PlaybackView>>,
    subscribers: Vec<(Weak<Mutex<Pending>>, watch::Sender<()>)>,
}

struct Pending {
    publication: Option<PlaybackProjection>,
    cleared: bool,
}

pub struct PlaybackSubscription {
    pending: Arc<Mutex<Pending>>,
    changed: watch::Receiver<()>,
}

impl PlaybackUpdates {
    pub fn current(&self) -> Option<Arc<PlaybackView>> {
        self.0
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .current
            .clone()
    }

    pub fn subscribe(&self) -> PlaybackSubscription {
        let mut updates = self.0.lock().unwrap_or_else(|p| p.into_inner());
        let pending = Arc::new(Mutex::new(Pending {
            publication: updates.current.as_ref().map(|view| PlaybackProjection {
                view: (**view).clone(),
                notices: Vec::new(),
            }),
            cleared: updates.current.is_none(),
        }));
        let (sender, changed) = watch::channel(());
        updates
            .subscribers
            .retain(|(pending, _)| pending.strong_count() > 0);
        updates.subscribers.push((Arc::downgrade(&pending), sender));
        PlaybackSubscription { pending, changed }
    }

    pub fn publish(&self, publication: PlaybackProjection) {
        let mut updates = self.0.lock().unwrap_or_else(|p| p.into_inner());
        updates.current = Some(Arc::new(publication.view.clone()));
        updates.subscribers.retain(|(pending, changed)| {
            let Some(pending) = pending.upgrade() else {
                return false;
            };
            let mut pending = pending.lock().unwrap_or_else(|p| p.into_inner());
            pending.cleared = false;
            if let Some(previous) = pending.publication.as_mut() {
                previous.view = publication.view.clone();
                previous.notices.extend(publication.notices.iter().cloned());
            } else {
                pending.publication = Some(publication.clone());
            }
            changed.send_replace(());
            true
        });
    }

    pub fn clear(&self) {
        let mut updates = self.0.lock().unwrap_or_else(|p| p.into_inner());
        updates.current = None;
        updates.subscribers.retain(|(pending, changed)| {
            let Some(pending) = pending.upgrade() else {
                return false;
            };
            let mut pending = pending.lock().unwrap_or_else(|p| p.into_inner());
            // Deliver unread notices before the cleared state; ordinary ticks
            // have no information to retain once playback is gone.
            if pending
                .publication
                .as_ref()
                .is_some_and(|p| p.notices.is_empty())
            {
                pending.publication = None;
            }
            pending.cleared = true;
            changed.send_replace(());
            true
        });
    }
}

impl PlaybackSubscription {
    /// `None` means playback is inactive; a closed publisher returns an error.
    pub async fn recv(&mut self) -> Result<Option<PlaybackProjection>, watch::error::RecvError> {
        loop {
            self.changed.borrow_and_update();
            {
                let mut pending = self.pending.lock().unwrap_or_else(|p| p.into_inner());
                if let Some(publication) = pending.publication.take() {
                    return Ok(Some(publication));
                }
                if std::mem::take(&mut pending.cleared) {
                    return Ok(None);
                }
            }
            self.changed.changed().await?;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        PlaybackNotice, PlaybackOutput, PlaybackSession, PlaybackSettings, RunId, Sequence,
    };

    #[tokio::test]
    async fn subscribers_coalesce_state_and_keep_their_own_ordered_notices() {
        let updates = PlaybackUpdates::default();
        let mut first = updates.subscribe();
        let mut slow = updates.subscribe();
        let mut view = PlaybackSession::new(
            Sequence::new(),
            "updates",
            PlaybackSettings::default(),
            PlaybackOutput::Local,
            false,
            1,
        )
        .view();
        let started = PlaybackNotice::RunStarted(RunId::new(1));
        updates.publish(PlaybackProjection {
            view: view.clone(),
            notices: vec![started.clone()],
        });
        assert_eq!(
            first.recv().await.unwrap().unwrap().notices,
            [started.clone()]
        );
        let failure = PlaybackNotice::OperationFailed("Unavailable".into());
        updates.publish(PlaybackProjection {
            view: view.clone(),
            notices: vec![failure.clone()],
        });
        for position in 1..1000 {
            view.transport.position_millis = position;
            updates.publish(PlaybackProjection {
                view: view.clone(),
                notices: vec![],
            });
        }
        let fast = first.recv().await.unwrap().unwrap();
        assert_eq!(fast.view, view);
        assert_eq!(fast.notices, [failure.clone()]);
        let delayed = slow.recv().await.unwrap().unwrap();
        assert_eq!(delayed.view, view);
        assert_eq!(delayed.notices, [started, failure]);
        drop(first);
        let mut late = updates.subscribe();
        let initial = late.recv().await.unwrap().unwrap();
        assert_eq!(initial.view, view);
        assert!(initial.notices.is_empty());
        assert_eq!(updates.current().as_deref(), Some(&view));
        updates.publish(PlaybackProjection {
            view: view.clone(),
            notices: vec![PlaybackNotice::OperationFailed("Stopped".into())],
        });
        updates.clear();
        assert!(updates.current().is_none());
        for subscriber in [&mut late, &mut slow] {
            assert_eq!(
                subscriber.recv().await.unwrap().unwrap().notices,
                [PlaybackNotice::OperationFailed("Stopped".into())]
            );
            assert!(subscriber.recv().await.unwrap().is_none());
        }
        let mut inactive = updates.subscribe();
        assert!(inactive.recv().await.unwrap().is_none());
        updates.publish(PlaybackProjection {
            view: view.clone(),
            notices: vec![],
        });
        for subscriber in [&mut late, &mut slow, &mut inactive] {
            assert_eq!(subscriber.recv().await.unwrap().unwrap().view, view);
        }
        drop(updates);
        assert!(late.recv().await.is_err());
        assert!(slow.recv().await.is_err());
    }
}
