use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use notify::{
    Event, EventKind, RecursiveMode, Watcher,
    event::{ModifyKind, RenameMode},
};
use tracing::warn;

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
        let (messages, receiver) = mpsc::channel();
        let listener = std::thread::current();
        let mut watcher = notify::recommended_watcher(move |event: notify::Result<Event>| {
            let message = match event {
                Ok(event) if !matches!(event.kind, EventKind::Access(_)) => {
                    FeedMessage::Event(event)
                }
                Ok(_) => return,
                Err(error) => FeedMessage::Failed(error.to_string()),
            };
            if messages.send(message).is_ok() {
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
            match wait_for_message(&receiver, POLL_INTERVAL, should_stop) {
                FeedWait::Message(FeedMessage::Event(event)) => {
                    let mut evidence = event_evidence(event);
                    loop {
                        match wait_for_message(&receiver, DEBOUNCE, should_stop) {
                            FeedWait::Message(FeedMessage::Event(event)) => {
                                evidence
                                    .merge(event_evidence(event))
                                    .expect("Local watcher emitted a non-Local change");
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
    Event(Event),
    Failed(String),
}

enum FeedWait {
    Message(FeedMessage),
    TimedOut,
    Stopped,
    Disconnected,
}

fn wait_for_message(
    receiver: &mpsc::Receiver<FeedMessage>,
    timeout: Duration,
    should_stop: &dyn Fn() -> bool,
) -> FeedWait {
    let deadline = Instant::now() + timeout;
    loop {
        if should_stop() {
            return FeedWait::Stopped;
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
        || matches!(event.kind, EventKind::Other)
        || matches!(
            event.kind,
            EventKind::Modify(ModifyKind::Name(mode))
                if mode != RenameMode::Both || event.paths.len() != 2
        );
    if complete_required {
        return LocalLiveChange::Rescan;
    }
    LocalLiveChange::Paths(event.paths.into_iter().collect())
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
            LocalLiveChange::Paths(vec![PathBuf::from("/music/one.flac")])
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
}
