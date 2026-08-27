use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use notify::{
    Event, EventKind, RecursiveMode, Watcher,
    event::{ModifyKind, RenameMode},
};
use tracing::warn;

use crate::source::LIVE_CHANGE_LIMIT;
use crate::{LocalLiveChange, SourceError, SourceResult};

const POLL_INTERVAL: Duration = Duration::from_millis(500);
const DEBOUNCE: Duration = Duration::from_secs(2);
const FAILED_ROOT_RETRY: Duration = Duration::from_secs(60);
const FEED_RETRY_MIN: Duration = Duration::from_secs(5);
const FEED_RETRY_MAX: Duration = Duration::from_secs(60);

pub struct LocalChangeFeed {
    roots: Vec<PathBuf>,
}

impl LocalChangeFeed {
    pub fn new(roots: Vec<PathBuf>) -> Self {
        Self { roots }
    }

    pub fn listen_forever(
        &self,
        on_ready: &mut dyn FnMut(bool) -> bool,
        on_change: &mut dyn FnMut(LocalLiveChange) -> bool,
        should_stop: &dyn Fn() -> bool,
    ) -> SourceResult<()> {
        let mut delay = FEED_RETRY_MIN;
        let mut reconnecting = false;
        while !should_stop() {
            let result = self.listen(reconnecting, on_ready, on_change, should_stop);
            if should_stop() {
                return Ok(());
            }
            if let Err(error) = result {
                warn!(%error, "Local library change feed disconnected");
            }
            reconnecting = true;
            if !wait_before_retry(delay, should_stop) {
                return Ok(());
            }
            delay = delay.saturating_mul(2).min(FEED_RETRY_MAX);
        }
        Ok(())
    }

    /// Run the one blocking filesystem feed.
    ///
    /// Rufin owns the source-session cancellation token and sends each result
    /// through Local's automatic/exact inventory operation.
    pub fn listen(
        &self,
        reconnecting: bool,
        on_ready: &mut dyn FnMut(bool) -> bool,
        on_change: &mut dyn FnMut(LocalLiveChange) -> bool,
        should_stop: &dyn Fn() -> bool,
    ) -> SourceResult<()> {
        let (messages, receiver) = mpsc::sync_channel(1);
        let overflow = Arc::new(AtomicBool::new(false));
        let callback_overflow = Arc::clone(&overflow);
        let listener = std::thread::current();
        let mut watcher = notify::recommended_watcher(move |event: notify::Result<Event>| {
            let message = match event {
                Ok(event) if !matches!(event.kind, EventKind::Access(_)) => {
                    FeedMessage::Change(event_evidence(event))
                }
                Ok(_) => return,
                Err(error) => FeedMessage::Failed(error.to_string()),
            };
            if admit_feed_message(&messages, &callback_overflow, message) {
                listener.unpark();
            }
        })
        .map_err(feed_error)?;

        let mut watched = 0;
        let mut failed_roots = Vec::new();
        for root in ordered_roots(self.roots.clone()) {
            match watcher.watch(&root, RecursiveMode::Recursive) {
                Ok(()) => watched += 1,
                Err(error) => {
                    warn!(%error, root = %root.display(), "failed to watch Local music folder");
                    failed_roots.push(root);
                }
            }
        }
        if watched == 0 {
            return Err(SourceError::Other(
                "No Local music folder could be watched.".to_string(),
            ));
        }
        if !on_ready(reconnecting) {
            return Ok(());
        }

        let mut retry_failed_roots_at = Instant::now() + FAILED_ROOT_RETRY;
        while !should_stop() {
            match wait_for_message(&receiver, &overflow, POLL_INTERVAL, should_stop) {
                FeedWait::Message(FeedMessage::Change(mut evidence)) => {
                    loop {
                        match wait_for_message(&receiver, &overflow, DEBOUNCE, should_stop) {
                            FeedWait::Message(FeedMessage::Change(incoming)) => {
                                evidence = evidence.merge(incoming);
                            }
                            FeedWait::Message(FeedMessage::Failed(error)) => {
                                return Err(SourceError::Other(error));
                            }
                            FeedWait::TimedOut => break,
                            FeedWait::Stopped => return Ok(()),
                            FeedWait::Disconnected => {
                                return Err(SourceError::Other(
                                    "Local music watcher disconnected.".to_string(),
                                ));
                            }
                        }
                    }
                    if !on_change(evidence) {
                        return Ok(());
                    }
                }
                FeedWait::Message(FeedMessage::Failed(error)) => {
                    return Err(SourceError::Other(error));
                }
                FeedWait::TimedOut => {}
                FeedWait::Stopped => return Ok(()),
                FeedWait::Disconnected => {
                    return Err(SourceError::Other(
                        "Local music watcher disconnected.".to_string(),
                    ));
                }
            }

            if !failed_roots.is_empty() && Instant::now() >= retry_failed_roots_at {
                let mut still_failed = Vec::new();
                let mut recovered = false;
                for root in failed_roots.drain(..) {
                    if should_stop() {
                        return Ok(());
                    }
                    match watcher.watch(&root, RecursiveMode::Recursive) {
                        Ok(()) => recovered = true,
                        Err(error) => {
                            warn!(%error, root = %root.display(), "failed to retry Local music folder");
                            still_failed.push(root);
                        }
                    }
                }
                failed_roots = still_failed;
                retry_failed_roots_at = Instant::now() + FAILED_ROOT_RETRY;
                if recovered && !on_change(LocalLiveChange::Rescan) {
                    return Ok(());
                }
            }
        }
        Ok(())
    }
}

fn wait_before_retry(delay: Duration, should_stop: &dyn Fn() -> bool) -> bool {
    let deadline = Instant::now() + delay;
    while !should_stop() {
        let now = Instant::now();
        if now >= deadline {
            return true;
        }
        std::thread::park_timeout(POLL_INTERVAL.min(deadline.saturating_duration_since(now)));
    }
    false
}

enum FeedMessage {
    Change(LocalLiveChange),
    Failed(String),
}

enum FeedWait {
    Message(FeedMessage),
    TimedOut,
    Stopped,
    Disconnected,
}

fn admit_feed_message(
    messages: &mpsc::SyncSender<FeedMessage>,
    overflow: &AtomicBool,
    message: FeedMessage,
) -> bool {
    match messages.try_send(message) {
        Ok(()) => true,
        Err(mpsc::TrySendError::Full(_)) => {
            overflow.store(true, Ordering::Release);
            true
        }
        Err(mpsc::TrySendError::Disconnected(_)) => false,
    }
}

fn wait_for_message(
    receiver: &mpsc::Receiver<FeedMessage>,
    overflow: &AtomicBool,
    timeout: Duration,
    should_stop: &dyn Fn() -> bool,
) -> FeedWait {
    let deadline = Instant::now() + timeout;
    loop {
        if should_stop() {
            return FeedWait::Stopped;
        }
        if overflow.swap(false, Ordering::AcqRel) {
            return FeedWait::Message(FeedMessage::Change(LocalLiveChange::Rescan));
        }
        match receiver.try_recv() {
            Ok(message) => return FeedWait::Message(message),
            Err(mpsc::TryRecvError::Disconnected) => return FeedWait::Disconnected,
            Err(mpsc::TryRecvError::Empty) => {}
        }
        let now = Instant::now();
        if now >= deadline {
            return FeedWait::TimedOut;
        }
        std::thread::park_timeout(deadline.saturating_duration_since(now));
    }
}

fn ordered_roots(roots: Vec<PathBuf>) -> Vec<PathBuf> {
    let mut seen = HashSet::new();
    roots
        .into_iter()
        .filter(|root| seen.insert(root.clone()))
        .collect()
}

fn feed_error(error: notify::Error) -> SourceError {
    SourceError::Other(error.to_string())
}

fn event_evidence(event: Event) -> LocalLiveChange {
    let complete_required = event.need_rescan()
        || event.paths.is_empty()
        || event.paths.len() > LIVE_CHANGE_LIMIT
        || matches!(event.kind, EventKind::Other)
        || matches!(
            event.kind,
            EventKind::Modify(ModifyKind::Name(mode))
                if mode != RenameMode::Both || event.paths.len() != 2
        );
    if complete_required {
        return LocalLiveChange::Rescan;
    }
    let rename = matches!(
        event.kind,
        EventKind::Modify(ModifyKind::Name(RenameMode::Both))
    )
    .then(|| (event.paths[0].clone(), event.paths[1].clone()));
    LocalLiveChange::Paths {
        paths: event.paths,
        rename,
    }
}

#[cfg(test)]
mod tests {
    use notify::event::{CreateKind, Flag};

    use super::*;

    #[test]
    fn ordinary_paths_are_exact() {
        let evidence = event_evidence(
            Event::new(EventKind::Create(CreateKind::File))
                .add_path(PathBuf::from("/music/one.flac")),
        );
        assert_eq!(
            evidence,
            LocalLiveChange::Paths {
                paths: vec![PathBuf::from("/music/one.flac")],
                rename: None,
            }
        );
    }

    #[test]
    fn partial_rename_and_rescan_require_complete_inventory() {
        let rename = event_evidence(
            Event::new(EventKind::Modify(ModifyKind::Name(RenameMode::From)))
                .add_path(PathBuf::from("/music/old.flac")),
        );
        let rescan = event_evidence(Event::new(EventKind::Any).set_flag(Flag::Rescan));
        assert_eq!(rename, LocalLiveChange::Rescan);
        assert_eq!(rescan, LocalLiveChange::Rescan);
    }

    #[test]
    fn complete_rename_keeps_explicit_old_and_new_evidence() {
        let old = PathBuf::from("/music/old.flac");
        let new = PathBuf::from("/music/new.flac");
        let rename = event_evidence(
            Event::new(EventKind::Modify(ModifyKind::Name(RenameMode::Both)))
                .add_path(old.clone())
                .add_path(new.clone()),
        );
        assert_eq!(
            rename,
            LocalLiveChange::Paths {
                paths: vec![old.clone(), new.clone()],
                rename: Some((old, new)),
            }
        );
    }

    #[test]
    fn debounce_evidence_is_bounded_by_one_rescan() {
        let current = LocalLiveChange::Paths {
            paths: (0..LIVE_CHANGE_LIMIT)
                .map(|index| PathBuf::from(format!("/music/{index}.flac")))
                .collect(),
            rename: None,
        };
        let merged = current.merge(LocalLiveChange::Paths {
            paths: vec![PathBuf::from("/music/overflow.flac")],
            rename: None,
        });
        assert_eq!(merged, LocalLiveChange::Rescan);
    }

    #[test]
    fn one_notify_event_cannot_exceed_the_exact_path_bound() {
        let mut event = Event::new(EventKind::Create(CreateKind::File));
        for index in 0..=LIVE_CHANGE_LIMIT {
            event = event.add_path(PathBuf::from(format!("/music/{index}.flac")));
        }
        assert_eq!(event_evidence(event), LocalLiveChange::Rescan);
    }

    #[test]
    fn full_ingress_keeps_one_message_and_one_rescan_flag() {
        let (sender, receiver) = mpsc::sync_channel(1);
        let overflow = AtomicBool::new(false);
        assert!(admit_feed_message(
            &sender,
            &overflow,
            FeedMessage::Change(LocalLiveChange::Paths {
                paths: vec![PathBuf::from("/music/one.flac")],
                rename: None,
            })
        ));
        assert!(admit_feed_message(
            &sender,
            &overflow,
            FeedMessage::Change(LocalLiveChange::Paths {
                paths: vec![PathBuf::from("/music/two.flac")],
                rename: None,
            })
        ));
        assert!(overflow.load(Ordering::Acquire));
        assert!(matches!(receiver.try_recv(), Ok(FeedMessage::Change(_))));
        assert!(matches!(
            receiver.try_recv(),
            Err(mpsc::TryRecvError::Empty)
        ));
    }
}
