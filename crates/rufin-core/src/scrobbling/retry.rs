use std::sync::mpsc::{Receiver, RecvTimeoutError, SyncSender, sync_channel};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

#[cfg(test)]
use library::ListenWrite;
use library::TrackMetadata;
use library::{Catalog, ListenDeliveryTarget, PendingListenDelivery, ReadCancellation};
use md5_digest::{Digest, Md5};
use reqwest::blocking::Client;
use tracing::warn;

use crate::scrobbling::services::{audioscrobbler, listenbrainz};
use crate::scrobbling::{AudioscrobblerSettings, ListenBrainzSettings, Settings};
use crate::settings::{SettingsFile, StoredSettings};

const NOTIFICATION_CAPACITY: usize = 1;
const DELIVERY_BATCH_SIZE: usize = 50;
const NOW_PLAYING_STABLE_DELAY: Duration = Duration::from_secs(1);
/// How long the worker waits after it could not read its work.
const ERROR_RETRY: Duration = Duration::from_secs(30);
pub(crate) const USER_AGENT: &str = concat!(
    "Rufin/",
    env!("CARGO_PKG_VERSION"),
    " (",
    env!("CARGO_PKG_REPOSITORY"),
    ")"
);

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct SubmissionTrack {
    pub(crate) title: String,
    pub(crate) artist: String,
    pub(crate) album: String,
    pub(crate) duration_millis: u64,
}

impl SubmissionTrack {
    fn capture(track: &TrackMetadata) -> Option<Self> {
        let title = track.title.trim();
        let artist = track.artist.trim();
        if title.is_empty() || artist.is_empty() {
            return None;
        }
        Some(Self {
            title: title.to_string(),
            artist: artist.to_string(),
            album: track.album.trim().to_string(),
            duration_millis: track.duration_millis.max(0) as u64,
        })
    }

    fn from_pending(pending: &PendingListenDelivery) -> Self {
        Self {
            title: pending.track_title.clone(),
            artist: pending.artist_name.clone(),
            album: pending.album_title.clone(),
            duration_millis: pending.duration_millis.max(0) as u64,
        }
    }
}

fn external_scrobble_threshold_millis(duration_millis: u64) -> Option<u64> {
    if duration_millis <= 30_000 {
        return None;
    }
    Some((duration_millis / 2).min(240_000))
}

#[cfg(test)]
mod duration_tests {
    use super::*;
    #[test]
    fn external_scrobbles_retain_the_service_duration_law() {
        assert_eq!(external_scrobble_threshold_millis(30_000), None);
        assert_eq!(external_scrobble_threshold_millis(31_000), Some(15_500));
        assert_eq!(external_scrobble_threshold_millis(180_000), Some(90_000));
        assert_eq!(external_scrobble_threshold_millis(900_000), Some(240_000));
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum Submission {
    NowPlaying(SubmissionTrack),
    Scrobble {
        track: SubmissionTrack,
        started_at_unix_seconds: i64,
    },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum DeliveryError {
    Retry(String),
    CredentialBlocked(String),
    Stop(String),
}

impl DeliveryError {
    pub(crate) fn retry(error: impl Into<String>) -> Self {
        Self::Retry(error.into())
    }

    pub(crate) fn credential_blocked(error: impl Into<String>) -> Self {
        Self::CredentialBlocked(error.into())
    }

    pub(crate) fn stop(error: impl Into<String>) -> Self {
        Self::Stop(error.into())
    }
}

impl std::fmt::Display for DeliveryError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Retry(error) | Self::CredentialBlocked(error) | Self::Stop(error) => {
                formatter.write_str(error)
            }
        }
    }
}

#[derive(Clone)]
enum TargetSettings {
    Audioscrobbler {
        service: audioscrobbler::Service,
        settings: AudioscrobblerSettings,
    },
    ListenBrainz(ListenBrainzSettings),
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
enum DeliveryService {
    LastFm,
    LibreFm,
    ListenBrainz,
}

impl DeliveryService {
    fn as_str(self) -> &'static str {
        match self {
            Self::LastFm => "lastfm",
            Self::LibreFm => "librefm",
            Self::ListenBrainz => "listenbrainz",
        }
    }

    fn parse(value: &str) -> Option<Self> {
        match value {
            "lastfm" => Some(Self::LastFm),
            "librefm" => Some(Self::LibreFm),
            "listenbrainz" => Some(Self::ListenBrainz),
            _ => None,
        }
    }
}

#[derive(Clone)]
struct DeliveryTarget {
    service: DeliveryService,
    account_id: String,
    settings: TargetSettings,
}

/// The accounts the worker sends for, with their secrets, and the committed settings they were
/// read from. The service switches and private mode are read from the settings when sending.
struct Accounts {
    settings: Settings,
    read_from: Option<Arc<StoredSettings>>,
}

struct DeliveryState {
    settings: SettingsFile,
    accounts: Mutex<Accounts>,
}

impl DeliveryState {
    fn accounts(&self) -> std::sync::MutexGuard<'_, Accounts> {
        self.accounts.lock().unwrap_or_else(|p| p.into_inner())
    }

    /// The accounts with the committed switches.
    fn current(&self, stored: &StoredSettings) -> Settings {
        let mut settings = self.accounts().settings.clone();
        settings.copy_switches(&stored.scrobbling);
        settings
    }

    /// What sends now, or None in private mode.
    fn sending(&self) -> Option<Settings> {
        let stored = self.settings.load();
        (!stored.ui.application.private_mode).then(|| self.current(&stored))
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum DeliveryFlow {
    Continue,
    StopAccount,
}

#[derive(Default)]
struct PendingWork {
    now_playing: Option<(SubmissionTrack, Instant)>,
    wake: bool,
    closed: bool,
}

fn take_stable_now_playing(
    pending: &mut Option<(SubmissionTrack, Instant)>,
    now: Instant,
) -> Option<SubmissionTrack> {
    let stable = pending
        .as_ref()
        .is_some_and(|(_, changed_at)| now >= *changed_at + NOW_PLAYING_STABLE_DELAY);
    stable.then(|| pending.take().expect("stable update must be pending").0)
}

/// The delivery thread, which runs while an account exists. It wakes for recorded listens,
/// for now-playing updates and when a deferred scrobble is due.
struct Worker {
    sender: SyncSender<()>,
    pending: Arc<Mutex<PendingWork>>,
    thread: Mutex<Option<JoinHandle<()>>>,
}

impl Worker {
    fn new(
        catalog: Catalog,
        runtime: tokio::runtime::Handle,
        state: Arc<DeliveryState>,
    ) -> Result<Self, String> {
        let (sender, receiver) = sync_channel(NOTIFICATION_CAPACITY);
        let pending = Arc::new(Mutex::new(PendingWork::default()));
        let worker_pending = Arc::clone(&pending);
        let thread_runtime = runtime;
        let thread = std::thread::Builder::new()
            .name("rufin-scrobbling".to_string())
            .spawn(move || {
                let client = match Client::builder()
                    .timeout(Duration::from_secs(6))
                    .user_agent(USER_AGENT)
                    .build()
                {
                    Ok(client) => client,
                    Err(error) => {
                        warn!(%error, "could not start external scrobbling");
                        return;
                    }
                };
                run_worker(
                    client,
                    catalog,
                    thread_runtime,
                    state,
                    receiver,
                    worker_pending,
                );
            })
            .map_err(|error| error.to_string())?;
        Ok(Self {
            sender,
            pending,
            thread: Mutex::new(Some(thread)),
        })
    }

    fn now_playing(&self, track: Option<SubmissionTrack>) {
        self.update(|pending| pending.now_playing = track.map(|track| (track, Instant::now())));
    }

    fn update(&self, update: impl FnOnce(&mut PendingWork)) {
        if let Ok(mut pending) = self.pending.lock() {
            if pending.closed {
                return;
            }
            update(&mut pending);
            drop(pending);
            let _ = self.sender.try_send(());
        }
    }

    fn wake(&self) {
        self.update(|pending| pending.wake = true);
    }

    fn shutdown(&self) {
        if let Ok(mut pending) = self.pending.lock() {
            pending.closed = true;
        }
        let _ = self.sender.try_send(());
        if let Some(thread) = self.thread.lock().unwrap_or_else(|p| p.into_inner()).take() {
            let _ = thread.join();
        }
    }
}

impl Drop for Worker {
    fn drop(&mut self) {
        self.shutdown();
    }
}

pub struct Scrobbler {
    catalog: Catalog,
    runtime: tokio::runtime::Handle,
    state: Arc<DeliveryState>,
    /// Present while an account exists.
    worker: Mutex<Option<Worker>>,
}

impl Scrobbler {
    pub fn shutdown(&self) {
        if let Some(worker) = self.worker.lock().unwrap_or_else(|p| p.into_inner()).take() {
            worker.shutdown();
        }
    }

    /// Starts the worker when an account exists and stops it when none does.
    fn follow_accounts(&self, accounts: bool) -> Result<(), String> {
        let mut worker = self.worker.lock().unwrap_or_else(|p| p.into_inner());
        if !accounts {
            if let Some(worker) = worker.take() {
                worker.shutdown();
            }
        } else if worker.is_none() {
            *worker = Some(Worker::new(
                self.catalog.clone(),
                self.runtime.clone(),
                Arc::clone(&self.state),
            )?);
        }
        Ok(())
    }

    /// Sends what waits, such as listens recorded since.
    pub fn wake(&self) {
        if let Some(worker) = &*self.worker.lock().unwrap_or_else(|p| p.into_inner()) {
            worker.wake();
        }
    }

    /// Starts without secrets; the owner reads them and installs the accounts.
    pub(crate) fn new(
        catalog: Catalog,
        runtime: tokio::runtime::Handle,
        settings: SettingsFile,
    ) -> Self {
        let mut accounts = settings.load().scrobbling_runtime_settings();
        accounts.sanitize();
        Self {
            catalog,
            runtime,
            state: Arc::new(DeliveryState {
                settings,
                accounts: Mutex::new(Accounts {
                    settings: accounts,
                    read_from: None,
                }),
            }),
            worker: Mutex::new(None),
        }
    }

    /// The accounts with their secrets and the committed switches.
    pub(crate) fn settings(&self) -> Settings {
        self.state.current(&self.state.settings.load())
    }

    /// Whether the accounts were read from settings that name other accounts or secrets.
    pub(crate) fn stale(&self, stored: &StoredSettings) -> bool {
        self.state
            .accounts()
            .read_from
            .as_ref()
            .is_none_or(|read_from| !read_from.same_scrobbling_accounts(stored))
    }

    /// Sends for `settings` from now on: the accounts with their secrets, read from
    /// `read_from`. Scrobbles waiting for an account that is gone are dropped, and those
    /// held for an account signed in again are sent.
    pub(crate) fn install(
        &self,
        mut settings: Settings,
        read_from: Arc<StoredSettings>,
    ) -> Result<(), String> {
        settings.sanitize();
        let (previous, current, reauthorized) = {
            let mut accounts = self.state.accounts();
            let previous = known_accounts(&accounts.settings);
            let current = known_accounts(&settings);
            let reauthorized = reauthorized_accounts(&accounts.settings, &settings);
            *accounts = Accounts {
                settings,
                read_from: Some(read_from),
            };
            (previous, current, reauthorized)
        };
        self.follow_accounts(!current.is_empty())?;
        for (service, account_id) in previous {
            if !current
                .iter()
                .any(|current| current == &(service, account_id.clone()))
            {
                self.runtime
                    .block_on(
                        self.catalog
                            .remove_listen_deliveries(service.as_str(), &account_id),
                    )
                    .map_err(|error| error.to_string())?;
            }
        }
        let now = unix_seconds();
        for (service, account_id) in reauthorized {
            self.runtime
                .block_on(
                    self.catalog
                        .wake_listen_deliveries(service.as_str(), &account_id, now),
                )
                .map_err(|error| error.to_string())?;
        }
        self.wake();
        Ok(())
    }

    pub fn now_playing(&self, track: Option<&TrackMetadata>) {
        let enabled = self
            .state
            .sending()
            .is_some_and(|settings| !targets(&settings, true).is_empty());
        if let Some(worker) = &*self.worker.lock().unwrap_or_else(|p| p.into_inner()) {
            worker.now_playing(track.filter(|_| enabled).and_then(SubmissionTrack::capture));
        }
    }

    pub fn listen_delivery_targets(
        &self,
        listen: &library::ListenWrite,
    ) -> Vec<ListenDeliveryTarget> {
        let Some(settings) = self.state.sending().filter(|_| !listen.skipped) else {
            return Vec::new();
        };
        if !external_scrobble_threshold_millis(listen.duration_millis.max(0) as u64)
            .is_some_and(|threshold| listen.listened_millis.max(0) as u64 >= threshold)
        {
            return Vec::new();
        }
        targets(&settings, false)
            .into_iter()
            .map(|target| ListenDeliveryTarget {
                service: target.service.as_str().to_string(),
                account_id: target.account_id,
                next_attempt_at: Some(unix_seconds()),
            })
            .collect()
    }
}

fn run_worker(
    client: Client,
    catalog: Catalog,
    runtime: tokio::runtime::Handle,
    state: Arc<DeliveryState>,
    receiver: Receiver<()>,
    pending: Arc<Mutex<PendingWork>>,
) {
    // Scrobbles left from earlier runs are looked at once at start.
    let mut retry_at = Some(Instant::now());
    loop {
        let now = Instant::now();
        let wake = match pending.lock() {
            Ok(mut pending) => {
                if pending.closed {
                    return;
                }
                std::mem::take(&mut pending.wake)
            }
            Err(_) => return,
        };
        if wake || retry_at.is_some_and(|at| now >= at) {
            retry_at = if deliver_due(&client, &catalog, &runtime, &state, &pending) {
                Some(Instant::now())
            } else {
                next_retry(&catalog, &runtime)
            };
            continue;
        }
        let (track, deadline) = match pending.lock() {
            Ok(mut pending) => {
                let track = take_stable_now_playing(&mut pending.now_playing, now);
                let deadline = pending
                    .now_playing
                    .as_ref()
                    .map(|(_, changed_at)| *changed_at + NOW_PLAYING_STABLE_DELAY)
                    .into_iter()
                    .chain(retry_at)
                    .min();
                (track, deadline)
            }
            Err(_) => return,
        };
        if let Some(track) = track {
            deliver_now_playing(&client, &state, &pending, track);
            continue;
        }
        let woken = match deadline {
            Some(deadline) => receiver
                .recv_timeout(deadline.saturating_duration_since(now))
                .or_else(|error| match error {
                    RecvTimeoutError::Timeout => Ok(()),
                    RecvTimeoutError::Disconnected => Err(()),
                }),
            None => receiver.recv().map_err(|_| ()),
        };
        if woken.is_err() {
            return;
        }
    }
}

/// When the next deferred scrobble is due. Scrobbles due now that the last pass left waiting,
/// in private mode or for a switched-off account, wait for the settings change that wakes the
/// worker; with none deferred, only a wake starts the next pass.
fn next_retry(catalog: &Catalog, runtime: &tokio::runtime::Handle) -> Option<Instant> {
    let now = unix_seconds();
    let next = match runtime.block_on(catalog.next_listen_delivery_at(now)) {
        Ok(next) => next?,
        Err(error) => {
            warn!(%error, "could not read external scrobbling work");
            return Some(Instant::now() + ERROR_RETRY);
        }
    };
    Some(Instant::now() + Duration::from_secs(next.saturating_sub(now).max(0) as u64))
}

fn deliver_now_playing(
    client: &Client,
    state: &DeliveryState,
    pending: &Mutex<PendingWork>,
    track: SubmissionTrack,
) {
    let targets = state
        .sending()
        .map(|settings| targets(&settings, true))
        .unwrap_or_default();
    let submission = Submission::NowPlaying(track);
    for target in targets {
        if pending.lock().map_or(true, |pending| pending.closed) {
            return;
        }
        if let Err(error) = submit(client, &target, &submission) {
            warn!(%error, service = ?target.service, "now-playing update failed");
        }
    }
}

/// One pass over the due scrobbles. Whether it settled any, so more may be due at once.
fn deliver_due(
    client: &Client,
    catalog: &Catalog,
    runtime: &tokio::runtime::Handle,
    state: &DeliveryState,
    work: &Mutex<PendingWork>,
) -> bool {
    if state.sending().is_none() {
        return false;
    }
    let now = unix_seconds();
    let cancellation = ReadCancellation::new();
    let pending = match runtime.block_on(catalog.due_listen_deliveries(
        now,
        DELIVERY_BATCH_SIZE,
        &cancellation,
    )) {
        Ok(pending) => pending,
        Err(error) => {
            warn!(%error, "could not read external scrobbling work");
            return false;
        }
    };
    let mut settled = false;
    let mut stopped = std::collections::HashSet::new();
    for pending in pending {
        if work.lock().map_or(true, |work| work.closed) {
            return false;
        }
        let Some(service) = DeliveryService::parse(&pending.service) else {
            let _ = runtime.block_on(catalog.complete_listen_delivery(pending.outbox_key));
            settled = true;
            continue;
        };
        let account = (service, pending.account_id.clone());
        if stopped.contains(&account) {
            continue;
        }
        let Some(current) = current_target(state, service, &pending.account_id) else {
            continue;
        };
        let submission = Submission::Scrobble {
            track: SubmissionTrack::from_pending(&pending),
            started_at_unix_seconds: pending.started_at,
        };
        match finish_delivery(
            catalog,
            runtime,
            pending,
            now,
            submit(client, &current, &submission),
        ) {
            DeliveryFlow::Continue => settled = true,
            DeliveryFlow::StopAccount => {
                stopped.insert(account);
            }
        }
    }
    settled
}

fn finish_delivery(
    catalog: &Catalog,
    runtime: &tokio::runtime::Handle,
    pending: PendingListenDelivery,
    now: i64,
    result: Result<(), DeliveryError>,
) -> DeliveryFlow {
    match result {
        Ok(()) => {
            if let Err(error) =
                runtime.block_on(catalog.complete_listen_delivery(pending.outbox_key))
            {
                warn!(%error, "could not complete external scrobbling work");
            }
            DeliveryFlow::Continue
        }
        Err(DeliveryError::Retry(error)) => {
            let service = pending.service.clone();
            let next_attempt_at = now.saturating_add(retry_delay(
                u32::try_from(pending.attempts).unwrap_or(u32::MAX),
            ));
            if let Err(store_error) = runtime.block_on(catalog.defer_listen_delivery(
                pending.outbox_key,
                next_attempt_at,
                Some(&error),
            )) {
                warn!(%store_error, "could not defer external scrobbling work");
            }
            warn!(
                %error,
                ?service,
                "external scrobble will be retried"
            );
            DeliveryFlow::StopAccount
        }
        Err(DeliveryError::CredentialBlocked(error)) => {
            let service = pending.service.clone();
            if let Err(store_error) = runtime.block_on(catalog.block_listen_deliveries(
                &pending.service,
                &pending.account_id,
                &error,
            )) {
                warn!(%store_error, "could not preserve credential-blocked scrobbles");
            }
            warn!(
                %error,
                ?service,
                "external scrobbling credentials need attention"
            );
            DeliveryFlow::StopAccount
        }
        Err(DeliveryError::Stop(error)) => {
            let service = pending.service.clone();
            warn!(
                %error,
                ?service,
                "external scrobble was rejected"
            );
            if let Err(store_error) =
                runtime.block_on(catalog.complete_listen_delivery(pending.outbox_key))
            {
                warn!(%store_error, "could not discard rejected external scrobble");
            }
            DeliveryFlow::Continue
        }
    }
}

fn current_target(
    state: &DeliveryState,
    service: DeliveryService,
    account_id: &str,
) -> Option<DeliveryTarget> {
    state.sending().and_then(|settings| {
        targets(&settings, false)
            .into_iter()
            .find(|target| target.service == service && target.account_id == account_id)
    })
}

fn submit(
    client: &Client,
    target: &DeliveryTarget,
    submission: &Submission,
) -> Result<(), DeliveryError> {
    match &target.settings {
        TargetSettings::Audioscrobbler { service, settings } => {
            audioscrobbler::submit(client, *service, settings, submission)
        }
        TargetSettings::ListenBrainz(settings) => {
            listenbrainz::submit(client, settings, submission)
        }
    }
}

fn targets(settings: &Settings, now_playing: bool) -> Vec<DeliveryTarget> {
    let mut targets = Vec::with_capacity(3);
    if settings.lastfm.configured(now_playing) {
        targets.push(DeliveryTarget {
            service: DeliveryService::LastFm,
            account_id: audioscrobbler_account_id(DeliveryService::LastFm, &settings.lastfm),
            settings: TargetSettings::Audioscrobbler {
                service: audioscrobbler::Service::LastFm,
                settings: settings.lastfm.clone(),
            },
        });
    }
    if settings.librefm.configured(now_playing) {
        targets.push(DeliveryTarget {
            service: DeliveryService::LibreFm,
            account_id: audioscrobbler_account_id(DeliveryService::LibreFm, &settings.librefm),
            settings: TargetSettings::Audioscrobbler {
                service: audioscrobbler::Service::LibreFm,
                settings: settings.librefm.clone(),
            },
        });
    }
    if settings.listenbrainz.configured(now_playing) {
        targets.push(DeliveryTarget {
            service: DeliveryService::ListenBrainz,
            account_id: opaque_account_id(
                DeliveryService::ListenBrainz,
                &settings.listenbrainz.user_token,
            ),
            settings: TargetSettings::ListenBrainz(settings.listenbrainz.clone()),
        });
    }
    targets
}

fn known_accounts(settings: &Settings) -> Vec<(DeliveryService, String)> {
    let mut accounts = Vec::with_capacity(3);
    if !settings.lastfm.session_key.is_empty() {
        accounts.push((
            DeliveryService::LastFm,
            audioscrobbler_account_id(DeliveryService::LastFm, &settings.lastfm),
        ));
    }
    if !settings.librefm.session_key.is_empty() {
        accounts.push((
            DeliveryService::LibreFm,
            audioscrobbler_account_id(DeliveryService::LibreFm, &settings.librefm),
        ));
    }
    if !settings.listenbrainz.user_token.is_empty() {
        accounts.push((
            DeliveryService::ListenBrainz,
            opaque_account_id(
                DeliveryService::ListenBrainz,
                &settings.listenbrainz.user_token,
            ),
        ));
    }
    accounts
}

fn reauthorized_accounts(
    previous: &Settings,
    current: &Settings,
) -> Vec<(DeliveryService, String)> {
    let mut accounts = Vec::with_capacity(2);
    for (service, previous, current) in [
        (DeliveryService::LastFm, &previous.lastfm, &current.lastfm),
        (
            DeliveryService::LibreFm,
            &previous.librefm,
            &current.librefm,
        ),
    ] {
        if previous.session_key.is_empty() || current.session_key.is_empty() {
            continue;
        }
        let previous_account = audioscrobbler_account_id(service, previous);
        let current_account = audioscrobbler_account_id(service, current);
        let credentials_changed = previous.api_key != current.api_key
            || previous.api_secret != current.api_secret
            || previous.session_key != current.session_key;
        if previous_account == current_account && credentials_changed {
            accounts.push((service, current_account));
        }
    }
    accounts
}

fn audioscrobbler_account_id(
    service: DeliveryService,
    settings: &AudioscrobblerSettings,
) -> String {
    let identity = if settings.username.trim().is_empty() {
        settings.session_key.as_str()
    } else {
        settings.username.as_str()
    };
    opaque_account_id(service, &identity.to_lowercase())
}

fn opaque_account_id(service: DeliveryService, identity: &str) -> String {
    let service = match service {
        DeliveryService::LastFm => "lastfm",
        DeliveryService::LibreFm => "librefm",
        DeliveryService::ListenBrainz => "listenbrainz",
    };
    let value = format!("rufin-scrobbling-account\0{service}\0{}", identity.trim());
    format!("{:x}", Md5::digest(value))
}

fn retry_delay(attempts: u32) -> i64 {
    let exponent = attempts.min(7);
    (30_i64.saturating_mul(1_i64 << exponent)).min(3_600)
}

fn unix_seconds() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs().min(i64::MAX as u64) as i64)
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use library::{Catalog, LibraryField, ReadCancellation, SourceId};

    use super::*;

    #[test]
    fn now_playing_keeps_only_the_track_stable_for_one_second() {
        let first = Instant::now();
        let mut pending = Some((submission_track("First"), first));
        assert!(
            take_stable_now_playing(&mut pending, first + Duration::from_millis(999)).is_none()
        );
        pending = Some((submission_track("Last"), first + Duration::from_millis(500)));
        assert_eq!(
            take_stable_now_playing(&mut pending, first + Duration::from_millis(1500))
                .map(|track| track.title),
            Some("Last".to_string())
        );
    }

    #[test]
    fn account_identity_is_service_scoped_and_does_not_store_the_secret() {
        let lastfm = opaque_account_id(DeliveryService::LastFm, "secret");
        let listenbrainz = opaque_account_id(DeliveryService::ListenBrainz, "secret");
        assert_ne!(lastfm, listenbrainz);
        assert!(!lastfm.contains("secret"));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn one_completed_play_owns_activity_and_each_delivery_target_once() {
        let directory = tempfile::tempdir().expect("temporary Store");
        let catalog = Catalog::open(directory.path().join("library.sqlite3"))
            .await
            .expect("open Catalog");
        catalog
            .reconcile_source(&library::SourceId::new("source"), "server", "Source")
            .await
            .unwrap();
        catalog
            .observe("source")
            .await
            .unwrap()
            .apply_catalog(
                &library::CatalogBatch {
                    tracks: vec![library::TrackObservation {
                        object_id: "track".into(),
                        metadata: library::TrackMetadata {
                            title: "Track".into(),
                            artist: "Artist".into(),
                            album: "Album".into(),
                            sort_text: "track".into(),
                            duration_millis: 180_000,
                            disc_number: 1,
                            track_number: 1,
                            source_format: Some("FLAC".into()),
                            ..Default::default()
                        },
                        audio_revision: Some([1; 32]),
                        artwork_binding: None,
                        ..Default::default()
                    }],
                    ..Default::default()
                },
                true,
            )
            .await
            .unwrap();
        let source = {
            catalog.loaded().await;
            catalog.source_key(&library::SourceId::new("source"))
        }
        .unwrap();
        let cancellation = ReadCancellation::new();
        let track_uri = catalog
            .selected_track_uris(
                &library::MemberRead {
                    collection: library::Collection::Source(source),
                    sort: Some(LibraryField::Title),
                    ..library::MemberRead::new(library::Collection::Source(source))
                }
                .selection(),
                None,
                &cancellation,
            )
            .await
            .expect("Track order")[0]
            .clone();
        let settings = Settings {
            lastfm: AudioscrobblerSettings {
                enabled: true,
                username: "listener".to_string(),
                api_key: "key".to_string(),
                api_secret: "secret".to_string(),
                session_key: "session".to_string(),
                now_playing_enabled: true,
            },
            listenbrainz: ListenBrainzSettings {
                enabled: true,
                username: String::new(),
                user_token: "token".to_string(),
                now_playing_enabled: true,
            },
            ..Settings::default()
        };
        let settings_file = SettingsFile::memory();
        settings_file
            .edit(|stored| {
                stored.scrobbling.copy_switches(&settings);
                Ok(())
            })
            .unwrap();
        let scrobbler = Scrobbler::new(
            catalog.clone(),
            tokio::runtime::Handle::current(),
            settings_file.clone(),
        );
        tokio::task::block_in_place(|| scrobbler.install(settings, settings_file.load()))
            .expect("install accounts");
        let track = catalog
            .track_references_for_uris(std::slice::from_ref(&track_uri), &cancellation)
            .await
            .expect("materialize Track")
            .pop()
            .expect("Track");
        let listen = ListenWrite {
            external_id: Some("play".to_string()),
            media_uri: track.media_uri.clone(),
            title: track.metadata.title.clone(),
            artist: track.metadata.artist.clone(),
            album: track.metadata.album.clone(),
            duration_millis: track.metadata.duration_millis,
            disc_number: Some(track.metadata.disc_number),
            track_number: Some(track.metadata.track_number),
            year: track.metadata.year,
            release_date: track.metadata.release_date.clone(),
            source_format: track.metadata.source_format.clone(),
            musicbrainz_recording_id: track.metadata.musicbrainz_recording_id.clone(),
            musicbrainz_release_track_id: track.metadata.musicbrainz_release_track_id.clone(),
            started_at: 100,
            local_period: "1970-01".to_string(),
            listened_millis: 90_000,
            skipped: false,
        };
        let deliveries = scrobbler.listen_delivery_targets(&listen);
        assert_eq!(deliveries.len(), 2);
        let mut early = listen.clone();
        early.listened_millis = 4_000;
        assert!(scrobbler.listen_delivery_targets(&early).is_empty());
        early = listen.clone();
        early.skipped = true;
        assert!(scrobbler.listen_delivery_targets(&early).is_empty());
        catalog
            .record_listen(&listen, None, &deliveries)
            .await
            .expect("record Activity");
        catalog
            .record_listen(&listen, None, &deliveries)
            .await
            .expect("record Activity idempotently");

        assert_eq!(
            catalog
                .order(
                    &library::Read::Members(library::MemberRead {
                        descending: true,
                        ..library::MemberRead::new(library::Collection::History {
                            source: Some(SourceId::new("source"))
                        })
                    }),
                    &cancellation
                )
                .await
                .expect("Activity")
                .len(),
            1
        );
        let due = catalog
            .due_listen_deliveries(i64::MAX, 10, &cancellation)
            .await
            .expect("delivery outbox");
        assert_eq!(due.len(), 2);
        let retry_track = SubmissionTrack::from_pending(&due[0]);
        assert_eq!(retry_track.duration_millis, 180_000);
        assert_eq!(due[0].listened_millis, 90_000);
        let lastfm_account = opaque_account_id(DeliveryService::LastFm, "listener");
        assert_eq!(
            catalog
                .block_listen_deliveries("lastfm", &lastfm_account, "invalid session")
                .await
                .expect("block account"),
            1
        );
        assert_eq!(
            catalog
                .due_listen_deliveries(i64::MAX, 10, &cancellation)
                .await
                .expect("blocked outbox")
                .len(),
            1
        );
        assert_eq!(
            catalog
                .wake_listen_deliveries("lastfm", &lastfm_account, 101)
                .await
                .expect("wake account"),
            1
        );
        assert_eq!(
            catalog
                .remove_listen_deliveries("lastfm", &lastfm_account)
                .await
                .expect("remove account"),
            1
        );
        assert_eq!(
            catalog
                .order(
                    &library::Read::Members(library::MemberRead {
                        descending: true,
                        ..library::MemberRead::new(library::Collection::History {
                            source: Some(SourceId::new("source"))
                        })
                    }),
                    &cancellation
                )
                .await
                .expect("Activity after account removal")
                .len(),
            1
        );
    }

    fn submission_track(title: &str) -> SubmissionTrack {
        SubmissionTrack {
            title: title.to_string(),
            artist: "Artist".to_string(),
            album: "Album".to_string(),
            duration_millis: 180_000,
        }
    }
}
