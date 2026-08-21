use super::*;
use std::time::Instant;

use tracing::info;

pub(super) struct ConfiguredJellyfinFeed {
    pub(super) source: Arc<Source>,
    pub(super) changes: Arc<Mutex<PendingChanges>>,
    handle: Option<tokio::task::AbortHandle>,
}

#[cfg(test)]
impl ConfiguredJellyfinFeed {
    pub(super) fn test(
        source: Arc<Source>,
        pending: Option<ObservedSourceChange>,
        connected: bool,
    ) -> Self {
        Self {
            source,
            changes: Arc::new(Mutex::new(PendingChanges::new(pending, connected))),
            handle: None,
        }
    }
}

pub(super) struct PendingChanges {
    pub(super) connected: bool,
    active: bool,
    pending: Option<ObservedSourceChange>,
}

impl PendingChanges {
    pub(super) const MAXIMUM_IDS: usize = 1024;

    pub(super) fn new(pending: Option<ObservedSourceChange>, connected: bool) -> Self {
        Self {
            connected,
            active: false,
            pending,
        }
    }

    pub(super) fn merge(&mut self, change: ObservedSourceChange) {
        if let Some(pending) = &mut self.pending {
            if pending.merge(change).is_err() {
                *pending = ObservedSourceChange::full();
            }
        } else {
            self.pending = Some(change);
        }
        if matches!(
            self.pending.as_ref(),
            Some(ObservedSourceChange::Jellyfin { upserts, removals })
                if upserts.len() + removals.len() > Self::MAXIMUM_IDS
        ) {
            self.pending = Some(ObservedSourceChange::full());
        }
    }

    pub(super) fn take(&mut self) -> Option<ObservedSourceChange> {
        if self.active || !self.connected {
            return None;
        }
        let pending = self.pending.take()?;
        self.active = true;
        Some(pending)
    }

    pub(super) fn finish(&mut self, retry: Option<ObservedSourceChange>) {
        self.active = false;
        if let Some(change) = retry {
            self.merge(change);
        }
    }
}

impl Drop for ConfiguredJellyfinFeed {
    fn drop(&mut self) {
        if let Some(handle) = self.handle.take() {
            handle.abort();
        }
    }
}

pub(super) struct ObservedChangeRun {
    changes: Arc<Mutex<PendingChanges>>,
    change: Option<ObservedSourceChange>,
}

impl ObservedChangeRun {
    #[cfg(test)]
    pub(super) fn test(changes: Arc<Mutex<PendingChanges>>, change: ObservedSourceChange) -> Self {
        Self {
            changes,
            change: Some(change),
        }
    }

    fn finish(mut self) {
        self.change = None;
        self.changes
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .finish(None);
    }
}

impl Drop for ObservedChangeRun {
    fn drop(&mut self) {
        let Some(change) = self.change.take() else {
            return;
        };
        self.changes
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .finish(Some(change));
    }
}

impl Shared {
    pub(super) fn configured_feed_source(&self, source_id: &SourceId) -> Option<Arc<Source>> {
        self.state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .jellyfin_feeds
            .get(source_id)
            .map(|feed| Arc::clone(&feed.source))
    }
}

pub(super) struct ActiveObserver {
    pub(super) qualifier: SourceQualifier,
    pub(super) cancelled: Arc<AtomicBool>,
    pub(super) completion: Option<tokio::sync::oneshot::Receiver<()>>,
    pub(super) handle: Option<std::thread::JoinHandle<()>>,
}

impl ActiveObserver {
    fn cancel(&self) {
        self.cancelled.store(true, Ordering::Release);
        if let Some(handle) = &self.handle {
            handle.thread().unpark();
        }
    }

    pub(super) async fn stop(mut self) {
        self.cancel();
        if let Some(completion) = self.completion.take() {
            let _ = completion.await;
        }
        if let Some(handle) = self.handle.take()
            && handle.join().is_err()
        {
            warn!("Local library watcher thread panicked");
        }
    }
}

impl Drop for ActiveObserver {
    fn drop(&mut self) {
        self.cancel();
    }
}

pub(super) struct RefreshRequest {
    pub(super) qualifier: SourceQualifier,
    pub(super) visible: AtomicBool,
    pub(super) started: AtomicBool,
    pub(super) announced: AtomicBool,
    pub(super) cancelled: Arc<AtomicBool>,
}

pub(super) struct FreshnessAdmission {
    next_check: tokio::time::Instant,
    pub(super) pending: Option<u64>,
}

impl FreshnessAdmission {
    pub(super) fn new(now: tokio::time::Instant) -> Self {
        Self {
            next_check: now,
            pending: None,
        }
    }

    pub(super) fn defer(&mut self, now: tokio::time::Instant) {
        self.next_check = now + SOURCE_CHECK_INTERVAL;
    }

    pub(super) fn admit(&mut self, token: u64, catch_up: bool, now: tokio::time::Instant) -> bool {
        if !catch_up && now < self.next_check {
            return false;
        }
        self.next_check = now + SOURCE_CHECK_INTERVAL;
        if self.pending.is_some() {
            return false;
        }
        self.pending = Some(token);
        true
    }

    pub(super) fn finish(&mut self, token: u64) {
        if self.pending == Some(token) {
            self.pending = None;
        }
    }

    pub(super) fn cancel(&mut self) {
        self.pending = None;
    }
}

struct PendingFreshnessCheck {
    shared: Weak<Shared>,
    token: u64,
}

impl Drop for PendingFreshnessCheck {
    fn drop(&mut self) {
        if let Some(shared) = self.shared.upgrade() {
            shared.finish_freshness_check(self.token);
        }
    }
}

impl SourceOwner {
    pub(super) fn request_manual_refresh(&self, source_id: SourceId) {
        if self
            .shared
            .selected()
            .is_some_and(|selected| selected.source_id() == &source_id)
        {
            self.request_refresh(source_id, true);
            return;
        }
        self.spawn_serialized(true, move |operations, cancelled| async move {
            if operations
                .shared
                .selected()
                .is_some_and(|selected| selected.source_id() == &source_id)
            {
                SourceOwner {
                    shared: Arc::clone(&operations.shared),
                }
                .request_refresh(source_id, true);
                return;
            }
            operations
                .shared
                .send_event(SourceEvent::Operation(SourceOperation::Refreshing {
                    source_id: source_id.clone(),
                    progress: initial_progress(),
                }))
                .await;
            let started = Instant::now();
            info!(%source_id, "manual source refresh started");
            let progress_id = source_id.clone();
            let progress = operations.progress(Arc::clone(&cancelled), move |progress| {
                Some(SourceOperation::Refreshing {
                    source_id: progress_id.clone(),
                    progress,
                })
            });
            let result = prepare_configured_refresh_candidate(
                &operations.shared,
                &source_id,
                progress,
                Arc::clone(&cancelled),
            )
            .await;
            if cancelled.load(Ordering::Acquire) {
                return;
            }
            let result = match result {
                Ok(prepared) => {
                    let change = prepared.change();
                    let acceptance_owner = Arc::clone(&operations.shared);
                    let _acceptance = acceptance_owner.acceptance_lane.lock().await;
                    if !operations.shared.protect_interruptible_commit(&cancelled) {
                        return;
                    }
                    blocking(move || prepared.accept().map_err(string_error))
                        .await
                        .map(|_| change)
                }
                Err(error) => Err(error),
            };
            if cancelled.load(Ordering::Acquire) {
                return;
            }
            match result {
                Ok(change) => {
                    info!(
                        %source_id,
                        ?change,
                        elapsed_ms = started.elapsed().as_millis(),
                        "manual source refresh finished"
                    );
                    operations
                        .shared
                        .send_event(SourceEvent::Operation(SourceOperation::Idle))
                        .await;
                }
                Err(error) => {
                    warn!(
                        %error,
                        %source_id,
                        elapsed_ms = started.elapsed().as_millis(),
                        "manual source refresh failed"
                    );
                    operations
                        .shared
                        .send_event(SourceEvent::Operation(SourceOperation::Failed {
                            source_id: Some(source_id),
                            message: error,
                            add_form: false,
                        }))
                        .await;
                }
            }
        });
    }

    pub(super) fn install_configured_jellyfin_feed(&self, source: Arc<Source>, cold: bool) {
        let source_id = source.source_id().clone();
        if self
            .shared
            .configured_feed_source(&source_id)
            .is_some_and(|current| Arc::ptr_eq(&current, &source))
        {
            return;
        }
        let (start, started) = tokio::sync::oneshot::channel();
        let changes = Arc::new(Mutex::new(PendingChanges::new(
            cold.then(ObservedSourceChange::full),
            false,
        )));
        let shared = Arc::downgrade(&self.shared);
        let ready_shared = shared.clone();
        let ready_id = source_id.clone();
        let ready_changes = Arc::clone(&changes);
        let gap_shared = shared.clone();
        let gap_changes = Arc::clone(&changes);
        let change_id = source_id.clone();
        let change_changes = Arc::clone(&changes);
        let socket_source = Arc::clone(&source);
        let handle = self.shared.runtime.spawn(async move {
            if started.await.is_err() {
                return;
            }
            let result = socket_source
                .listen_jellyfin_changes(
                    move || {
                        ready_changes
                            .lock()
                            .unwrap_or_else(|poisoned| poisoned.into_inner())
                            .connected = true;
                        ready_shared.upgrade().is_some_and(|shared| {
                            SourceOwner { shared }.resume_configured_feed(&ready_id);
                            true
                        })
                    },
                    move || {
                        let mut changes = gap_changes
                            .lock()
                            .unwrap_or_else(|poisoned| poisoned.into_inner());
                        changes.connected = false;
                        changes.merge(ObservedSourceChange::full());
                        gap_shared.upgrade().is_some()
                    },
                    move |change| {
                        change_changes
                            .lock()
                            .unwrap_or_else(|poisoned| poisoned.into_inner())
                            .merge(change);
                        shared.upgrade().is_some_and(|shared| {
                            SourceOwner { shared }.resume_configured_feed(&change_id);
                            true
                        })
                    },
                )
                .await;
            if let Err(error) = result {
                warn!(%error, %source_id, "configured Jellyfin change feed stopped");
            }
        });
        let feed = ConfiguredJellyfinFeed {
            source,
            changes,
            handle: Some(handle.abort_handle()),
        };
        let previous = self
            .shared
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .jellyfin_feeds
            .insert(feed.source.source_id().clone(), feed);
        drop(previous);
        let _ = start.send(());
    }

    pub(super) fn remove_configured_feed(&self, source_id: &SourceId) {
        self.shared
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .jellyfin_feeds
            .remove(source_id);
    }

    pub(super) fn clear_configured_feeds(&self) {
        self.shared
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .jellyfin_feeds
            .clear();
    }

    pub(super) fn begin_configured_baseline(&self, source_id: &SourceId) {
        let state = self
            .shared
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let Some(feed) = state.jellyfin_feeds.get(source_id) else {
            return;
        };
        let mut changes = feed
            .changes
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if !changes.connected {
            changes.merge(ObservedSourceChange::full());
        }
    }

    pub(super) fn resume_configured_feed(&self, source_id: &SourceId) {
        let work = {
            let state = self
                .shared
                .state
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            let Some(feed) = state.jellyfin_feeds.get(source_id).filter(|_| {
                state.selected_revealed
                    && state
                        .selected
                        .as_ref()
                        .is_some_and(|selected| selected.current.source_id() == source_id)
            }) else {
                return;
            };
            (Arc::clone(&feed.source), Arc::clone(&feed.changes))
        };
        let (source, changes) = work;
        let change = {
            let mut state = changes
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            let Some(change) = state.take() else {
                return;
            };
            change
        };
        let registration = self.shared.reserve_interruptible();
        let cancelled = Arc::new(AtomicBool::new(false));
        self.shared
            .register_reserved_interruptible(registration, Arc::clone(&cancelled));
        let run = ObservedChangeRun {
            changes,
            change: Some(change.clone()),
        };
        let source_id = source_id.clone();
        self.spawn_registered(
            Some(registration),
            cancelled,
            move |mut operations, cancelled| async move {
                let Some(selected) = operations
                    .shared
                    .selected()
                    .filter(|selected| selected.source_id() == &source_id)
                else {
                    return;
                };
                if !operations
                    .shared
                    .configured_feed_source(&source_id)
                    .is_some_and(|current| Arc::ptr_eq(&current, &source))
                {
                    return;
                }
                if operations
                    .accept_observed_change(selected, change, cancelled)
                    .await
                {
                    run.finish();
                    operations.resume_configured_feed(&source_id);
                }
            },
        );
    }

    pub(super) fn request_refresh(&self, source_id: SourceId, visible: bool) {
        self.request_refresh_while_active(source_id, visible, None);
    }

    pub(super) fn request_refresh_while_active(
        &self,
        source_id: SourceId,
        visible: bool,
        parent_cancelled: Option<&AtomicBool>,
    ) {
        let Some(selected) = self
            .shared
            .selected()
            .filter(|selected| selected.source_id() == &source_id)
        else {
            return;
        };
        let qualifier = selected.qualifier();
        let request = {
            let mut state = self
                .shared
                .state
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            if parent_cancelled.is_some_and(|cancelled| cancelled.load(Ordering::Acquire)) {
                return;
            }
            if let Some(refresh) = state
                .refresh
                .as_ref()
                .filter(|refresh| refresh.qualifier == qualifier)
            {
                if visible {
                    refresh.visible.store(true, Ordering::Release);
                    if refresh.started.load(Ordering::Acquire)
                        && !refresh.announced.swap(true, Ordering::AcqRel)
                    {
                        let _ = self.shared.outputs.events.try_send(SourceEvent::Operation(
                            SourceOperation::Refreshing {
                                source_id: refresh.qualifier.source_id.clone(),
                                progress: initial_progress(),
                            },
                        ));
                    }
                }
                None
            } else {
                let request = Arc::new(RefreshRequest {
                    qualifier,
                    visible: AtomicBool::new(visible),
                    started: AtomicBool::new(false),
                    announced: AtomicBool::new(false),
                    cancelled: Arc::new(AtomicBool::new(false)),
                });
                let registration = self
                    .shared
                    .register_interruptible(Arc::clone(&request.cancelled));
                state.refresh = Some(Arc::clone(&request));
                Some((request, registration))
            }
        };
        let Some((request, registration)) = request else {
            return;
        };
        let request_for_work = Arc::clone(&request);
        self.spawn_registered(
            Some(registration),
            Arc::clone(&request.cancelled),
            move |mut operations, cancelled| async move {
                operations.refresh(request_for_work, cancelled).await;
            },
        );
    }

    pub(super) fn request_freshness_check(&self, catch_up: bool) {
        let Some(session) = self.shared.selected_session() else {
            return;
        };
        let Some(selected) = session.resolve().filter(|selected| {
            selected.source.is_some() && selected.configuration.kind != "jellyfin"
        }) else {
            return;
        };
        let qualifier = selected.qualifier();
        let cancelled = Arc::new(AtomicBool::new(false));
        let registration = self.shared.reserve_interruptible();
        let registration = {
            let mut state = self
                .shared
                .state
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            if !state
                .freshness
                .admit(registration.token, catch_up, tokio::time::Instant::now())
            {
                return;
            }
            self.shared
                .register_reserved_interruptible(registration, Arc::clone(&cancelled));
            registration
        };
        let pending = PendingFreshnessCheck {
            shared: Arc::downgrade(&self.shared),
            token: registration.token,
        };
        self.spawn_registered(
            Some(registration),
            cancelled,
            move |mut operations, cancelled| async move {
                let _pending = pending;
                let Some(selected) = session
                    .resolve()
                    .filter(|selected| selected.qualifier() == qualifier)
                else {
                    return;
                };
                operations.check_freshness(selected, cancelled).await;
            },
        );
    }
    pub(super) fn queue_observed_change(
        &self,
        changes: &Arc<Mutex<PendingChanges>>,
        session: &Arc<ActiveSource>,
        observer_cancelled: &Arc<AtomicBool>,
        change: ObservedSourceChange,
    ) -> bool {
        if resolve_observer_session(observer_cancelled, session).is_none() {
            return false;
        }
        changes
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .merge(change);
        self.resume_observed_changes(
            Arc::clone(changes),
            Arc::clone(session),
            Arc::clone(observer_cancelled),
        );
        true
    }

    fn resume_observed_changes(
        &self,
        changes: Arc<Mutex<PendingChanges>>,
        session: Arc<ActiveSource>,
        observer_cancelled: Arc<AtomicBool>,
    ) {
        if resolve_observer_session(&observer_cancelled, &session).is_none() {
            return;
        }
        let Some(change) = changes
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .take()
        else {
            return;
        };
        let registration = self.shared.reserve_interruptible();
        let cancelled = Arc::new(AtomicBool::new(false));
        self.shared
            .register_reserved_interruptible(registration, Arc::clone(&cancelled));
        let run = ObservedChangeRun {
            changes: Arc::clone(&changes),
            change: Some(change.clone()),
        };
        self.spawn_registered(
            Some(registration),
            cancelled,
            move |mut operations, cancelled| async move {
                let Some(selected) = resolve_observer_session(&observer_cancelled, &session) else {
                    return;
                };
                if operations
                    .accept_observed_change(selected, change, Arc::clone(&cancelled))
                    .await
                {
                    run.finish();
                    operations.resume_observed_changes(changes, session, observer_cancelled);
                }
            },
        );
    }

    pub(super) async fn stop_observer(&mut self) {
        let observer = self
            .shared
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .observer
            .take();
        if let Some(observer) = observer {
            observer.stop().await;
        }
    }

    pub(super) async fn refresh(
        &mut self,
        request: Arc<RefreshRequest>,
        cancelled: Arc<AtomicBool>,
    ) {
        let Some(selected) = self
            .shared
            .selected()
            .filter(|selected| selected.qualifier() == request.qualifier)
        else {
            self.shared.finish_refresh(&request);
            return;
        };
        let source_id = selected.source_id().clone();
        let started = Instant::now();
        request.started.store(true, Ordering::Release);
        if request.visible.load(Ordering::Acquire)
            && !request.announced.swap(true, Ordering::AcqRel)
        {
            info!(%source_id, "manual source refresh started");
            self.shared
                .send_event(SourceEvent::Operation(SourceOperation::Refreshing {
                    source_id: source_id.clone(),
                    progress: initial_progress(),
                }))
                .await;
        }
        let visible = Arc::clone(&request);
        let progress_source_id = source_id.clone();
        let progress = self.progress(Arc::clone(&cancelled), move |progress| {
            visible
                .visible
                .load(Ordering::Acquire)
                .then(|| SourceOperation::Refreshing {
                    source_id: progress_source_id.clone(),
                    progress,
                })
        });
        let prepared = prepare_refresh_candidate(
            Arc::clone(&self.shared),
            (*selected).clone(),
            progress,
            Arc::clone(&cancelled),
        )
        .await;
        let candidate_change = prepared.as_ref().ok().map(PreparedSourceCandidate::change);
        if cancelled.load(Ordering::Acquire) {
            self.shared.finish_refresh(&request);
            return;
        }
        let result = match prepared {
            Ok(prepared) => {
                let acceptance_owner = Arc::clone(&self.shared);
                let _acceptance = acceptance_owner.acceptance_lane.lock().await;
                if !self.shared.protect_interruptible_commit(&cancelled) {
                    self.shared.finish_refresh(&request);
                    return;
                }
                self.commit_refresh(Arc::clone(&selected), prepared).await
            }
            Err(error) => Err(error),
        };
        if cancelled.load(Ordering::Acquire) {
            self.shared.finish_refresh(&request);
            return;
        }
        let visible = self.shared.finish_refresh(&request).unwrap_or(false);
        match result {
            Ok(()) if visible => {
                info!(
                    %source_id,
                    ?candidate_change,
                    elapsed_ms = started.elapsed().as_millis(),
                    "manual source refresh finished"
                );
                self.shared
                    .send_event(SourceEvent::Operation(SourceOperation::Idle))
                    .await;
            }
            Ok(()) => {}
            Err(error) => {
                if visible {
                    warn!(
                        %error,
                        %source_id,
                        elapsed_ms = started.elapsed().as_millis(),
                        "manual source refresh failed"
                    );
                }
                self.refresh_failed(&selected, visible, error).await;
            }
        }
    }

    pub(super) async fn refresh_failed(
        &self,
        selected: &SelectedSourceState,
        visible: bool,
        error: String,
    ) {
        if visible {
            self.shared
                .send_event(SourceEvent::Operation(SourceOperation::Failed {
                    source_id: Some(selected.source_id().clone()),
                    message: error,
                    add_form: false,
                }))
                .await;
        } else {
            warn!(%error, "background source refresh failed");
        }
    }

    pub(super) async fn accept_observed_change(
        &mut self,
        selected: Arc<SelectedSourceState>,
        change: ObservedSourceChange,
        cancelled: Arc<AtomicBool>,
    ) -> bool {
        let Some(source) = selected.source.as_ref().cloned() else {
            return false;
        };
        let result: Result<(), String> = async {
            let progress: Arc<dyn Fn(SourceReadProgress) + Send + Sync> =
                Arc::new(|_: SourceReadProgress| {});
            let prepared = source
                .prepare_change(
                    Arc::clone(&selected.library),
                    change,
                    Arc::clone(&progress),
                    Arc::clone(&cancelled),
                )
                .await
                .map_err(string_error)?;
            let prepared = match prepared {
                PreparedSourceChange::Full => {
                    let mut context = (*selected).clone();
                    context.source = Some(source);
                    let candidate = prepare_refresh_candidate(
                        Arc::clone(&self.shared),
                        context,
                        progress,
                        Arc::clone(&cancelled),
                    )
                    .await?;
                    let acceptance_owner = Arc::clone(&self.shared);
                    let _acceptance = acceptance_owner.acceptance_lane.lock().await;
                    if !self.shared.protect_interruptible_commit(&cancelled) {
                        return Err("the source update was interrupted".to_string());
                    }
                    return self.commit_refresh(selected, candidate).await;
                }
                PreparedSourceChange::Ignored => return Ok(()),
                prepared => prepared,
            };
            let acceptance_owner = Arc::clone(&self.shared);
            let _acceptance = acceptance_owner.acceptance_lane.lock().await;
            if !self.shared.protect_interruptible_commit(&cancelled) {
                return Err("the source update was interrupted".to_string());
            }
            self.accept_prepared_change(selected, prepared).await
        }
        .await;
        match result {
            Ok(()) => true,
            Err(error) => {
                warn!(%error, "background selected source update failed");
                false
            }
        }
    }

    pub(super) async fn check_freshness(
        &mut self,
        selected: Arc<SelectedSourceState>,
        cancelled: Arc<AtomicBool>,
    ) {
        let Some(source) = selected.source.as_ref() else {
            return;
        };
        let freshness = match selected.library.provider_freshness().map_err(string_error) {
            Ok(freshness) => freshness,
            Err(error) => {
                warn!(%error, "could not check selected source freshness");
                return;
            }
        };
        match source.check_freshness(freshness.as_ref()).await {
            Ok(SourceFreshness::Changed(_)) => {
                SourceOwner {
                    shared: Arc::clone(&self.shared),
                }
                .request_refresh_while_active(
                    selected.source_id().clone(),
                    false,
                    Some(&cancelled),
                );
            }
            Ok(
                SourceFreshness::Unavailable | SourceFreshness::Unchanged | SourceFreshness::Busy,
            ) => {}
            Err(error) => warn!(%error, "could not check selected source freshness"),
        }
    }
}
