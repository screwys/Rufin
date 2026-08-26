use std::sync::mpsc::{Receiver, RecvTimeoutError, SyncSender, sync_channel};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

#[cfg(test)]
use library::ListenWrite;
use library::{Database, ListenDeliveryTarget, PendingListenDelivery, ReadCancellation};
use playback::{ListeningTrack, external_scrobble_threshold_millis};
use reqwest::blocking::Client;
use tracing::warn;

use crate::services::{audioscrobbler, listenbrainz};
use crate::{AudioscrobblerSettings, ListenBrainzSettings, Settings};

const NOTIFICATION_CAPACITY: usize = 1;
const DELIVERY_BATCH_SIZE: usize = 50;
const NOW_PLAYING_STABLE_DELAY: Duration = Duration::from_secs(1);
const RETRY_POLL: Duration = Duration::from_secs(30);
const USER_AGENT: &str = concat!("Rufin/", env!("CARGO_PKG_VERSION"));

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct SubmissionTrack {
    pub(crate) title: String,
    pub(crate) artist: String,
    pub(crate) album: String,
    pub(crate) duration_millis: u64,
}

impl SubmissionTrack {
    fn capture(track: &ListeningTrack) -> Option<Self> {
        let title = track.title.trim();
        let artists = track
            .artists
            .iter()
            .map(|artist| artist.trim())
            .filter(|artist| !artist.is_empty())
            .collect::<Vec<_>>();
        if title.is_empty() || artists.is_empty() {
            return None;
        }
        Some(Self {
            title: title.to_string(),
            artist: artists.join(", "),
            album: track
                .album
                .as_deref()
                .unwrap_or_default()
                .trim()
                .to_string(),
            duration_millis: track.duration_millis,
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

#[derive(Clone)]
struct DeliveryState {
    settings: Settings,
    private_mode: bool,
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

struct Worker {
    sender: SyncSender<()>,
    pending: Arc<Mutex<PendingWork>>,
    _thread: JoinHandle<()>,
}

impl Worker {
    fn new(
        database: Database,
        runtime: tokio::runtime::Handle,
        state: Arc<Mutex<DeliveryState>>,
    ) -> Result<Self, String> {
        let client = Client::builder()
            .timeout(Duration::from_secs(6))
            .user_agent(USER_AGENT)
            .build()
            .map_err(|error| error.to_string())?;
        let (sender, receiver) = sync_channel(NOTIFICATION_CAPACITY);
        let pending = Arc::new(Mutex::new(PendingWork::default()));
        let worker_pending = Arc::clone(&pending);
        let thread = std::thread::Builder::new()
            .name("rufin-scrobbling".to_string())
            .spawn(move || run_worker(client, database, runtime, state, receiver, worker_pending))
            .map_err(|error| error.to_string())?;
        Ok(Self {
            sender,
            pending,
            _thread: thread,
        })
    }

    fn now_playing(&self, track: SubmissionTrack) {
        self.update(|pending| pending.now_playing = Some((track, Instant::now())));
    }

    fn update(&self, update: impl FnOnce(&mut PendingWork)) {
        if let Ok(mut pending) = self.pending.lock() {
            update(&mut pending);
            drop(pending);
            let _ = self.sender.try_send(());
        }
    }

    fn wake(&self) {
        self.update(|pending| pending.wake = true);
    }
}

pub struct Scrobbler {
    database: Database,
    runtime: tokio::runtime::Handle,
    state: Arc<Mutex<DeliveryState>>,
    worker: Worker,
}

impl Scrobbler {
    pub fn new(
        database: Database,
        runtime: tokio::runtime::Handle,
        mut settings: Settings,
        private_mode: bool,
    ) -> Result<Self, String> {
        settings.sanitize();
        let state = Arc::new(Mutex::new(DeliveryState {
            settings,
            private_mode,
        }));
        let worker = Worker::new(database.clone(), runtime.clone(), Arc::clone(&state))?;
        Ok(Self {
            database,
            runtime,
            state,
            worker,
        })
    }

    pub fn update_settings(
        &self,
        mut settings: Settings,
        private_mode: bool,
    ) -> Result<(), String> {
        settings.sanitize();
        let (previous_accounts, current_accounts, reauthorized) = {
            let mut state = self
                .state
                .lock()
                .map_err(|_| "scrobbling settings lock was poisoned".to_string())?;
            let previous = known_accounts(&state.settings);
            let current = known_accounts(&settings);
            let reauthorized = reauthorized_accounts(&state.settings, &settings);
            state.settings = settings;
            state.private_mode = private_mode;
            (previous, current, reauthorized)
        };
        for (service, account_id) in previous_accounts {
            if !current_accounts
                .iter()
                .any(|current| current == &(service, account_id.clone()))
            {
                self.runtime
                    .block_on(
                        self.database
                            .remove_listen_deliveries(service.as_str(), &account_id),
                    )
                    .map_err(|error| error.to_string())?;
            }
        }
        let now = unix_seconds();
        for (service, account_id) in reauthorized {
            self.runtime
                .block_on(
                    self.database
                        .wake_listen_deliveries(service.as_str(), &account_id, now),
                )
                .map_err(|error| error.to_string())?;
        }
        if !private_mode {
            self.worker.wake();
        }
        Ok(())
    }

    pub fn now_playing(&self, track: &ListeningTrack) {
        let enabled = self
            .state
            .lock()
            .is_ok_and(|state| !state.private_mode && !targets(&state.settings, true).is_empty());
        if enabled && let Some(track) = SubmissionTrack::capture(track) {
            self.worker.now_playing(track);
        }
    }

    pub fn listen_delivery_targets(&self, track: &ListeningTrack) -> Vec<ListenDeliveryTarget> {
        let targets = {
            let state = self.state.lock().ok();
            let Some(state) = state else {
                return Vec::new();
            };
            if state.private_mode {
                return Vec::new();
            }
            if external_scrobble_threshold_millis(track.duration_millis).is_some() {
                targets(&state.settings, false)
            } else {
                Vec::new()
            }
        };
        targets
            .into_iter()
            .map(|target| ListenDeliveryTarget {
                service: target.service.as_str().to_string(),
                account_id: target.account_id,
                next_attempt_at: Some(unix_seconds()),
            })
            .collect()
    }

    pub fn listen_recorded(&self, delivery_count: usize) {
        if delivery_count > 0 {
            self.worker.wake();
        }
    }
}

fn run_worker(
    client: Client,
    database: Database,
    runtime: tokio::runtime::Handle,
    state: Arc<Mutex<DeliveryState>>,
    receiver: Receiver<()>,
    pending: Arc<Mutex<PendingWork>>,
) {
    let mut retry_at = Instant::now() + RETRY_POLL;
    loop {
        let now = Instant::now();
        let wake = pending
            .lock()
            .is_ok_and(|mut pending| std::mem::take(&mut pending.wake));
        if wake || now >= retry_at {
            deliver_due(&client, &database, &runtime, &state);
            retry_at = Instant::now() + RETRY_POLL;
            continue;
        }
        let (track, deadline) = match pending.lock() {
            Ok(mut pending) => {
                let track = take_stable_now_playing(&mut pending.now_playing, now);
                let deadline = pending
                    .now_playing
                    .as_ref()
                    .map(|(_, changed_at)| (*changed_at + NOW_PLAYING_STABLE_DELAY).min(retry_at))
                    .unwrap_or(retry_at);
                (track, deadline)
            }
            Err(_) => return,
        };
        if let Some(track) = track {
            deliver_now_playing(&client, &state, track);
            continue;
        }
        match receiver.recv_timeout(deadline.saturating_duration_since(now)) {
            Ok(()) | Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => return,
        }
    }
}

fn deliver_now_playing(client: &Client, state: &Arc<Mutex<DeliveryState>>, track: SubmissionTrack) {
    let targets = state
        .lock()
        .ok()
        .filter(|state| !state.private_mode)
        .map(|state| targets(&state.settings, true))
        .unwrap_or_default();
    let submission = Submission::NowPlaying(track);
    for target in targets {
        if let Err(error) = submit(client, &target, &submission) {
            warn!(%error, service = ?target.service, "now-playing update failed");
        }
    }
}

fn deliver_due(
    client: &Client,
    database: &Database,
    runtime: &tokio::runtime::Handle,
    state: &Arc<Mutex<DeliveryState>>,
) {
    if state.lock().map_or(true, |state| state.private_mode) {
        return;
    }
    let now = unix_seconds();
    let cancellation = ReadCancellation::new();
    let pending = match runtime.block_on(database.due_listen_deliveries(
        now,
        DELIVERY_BATCH_SIZE,
        &cancellation,
    )) {
        Ok(pending) => pending,
        Err(error) => {
            warn!(%error, "could not read external scrobbling work");
            return;
        }
    };
    let mut stopped = std::collections::HashSet::new();
    for pending in pending {
        let Some(service) = DeliveryService::parse(&pending.service) else {
            let _ = runtime.block_on(database.complete_listen_delivery(pending.outbox_key));
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
        if finish_delivery(
            database,
            runtime,
            pending,
            now,
            submit(client, &current, &submission),
        ) == DeliveryFlow::StopAccount
        {
            stopped.insert(account);
        }
    }
}

fn finish_delivery(
    database: &Database,
    runtime: &tokio::runtime::Handle,
    pending: PendingListenDelivery,
    now: i64,
    result: Result<(), DeliveryError>,
) -> DeliveryFlow {
    match result {
        Ok(()) => {
            if let Err(error) =
                runtime.block_on(database.complete_listen_delivery(pending.outbox_key))
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
            if let Err(store_error) = runtime.block_on(database.defer_listen_delivery(
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
            if let Err(store_error) = runtime.block_on(database.block_listen_deliveries(
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
                runtime.block_on(database.complete_listen_delivery(pending.outbox_key))
            {
                warn!(%store_error, "could not discard rejected external scrobble");
            }
            DeliveryFlow::Continue
        }
    }
}

fn current_target(
    state: &Arc<Mutex<DeliveryState>>,
    service: DeliveryService,
    account_id: &str,
) -> Option<DeliveryTarget> {
    state
        .lock()
        .ok()
        .filter(|state| !state.private_mode)
        .and_then(|state| {
            targets(&state.settings, false)
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
    format!("{:x}", md5::compute(value))
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
    use library::{Database, ReadCancellation, Scan, ScanOutcome, TrackSort};

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
        let database = Database::open(directory.path().join("library.sqlite3"))
            .await
            .expect("open Database");
        let mut scan = Scan::begin(&database, "source", "Source", "source", None)
            .await
            .expect("begin Scan");
        scan.write_track(
            "track",
            None,
            "Track",
            "track artist",
            "Album",
            "Artist",
            "track",
            180_000,
            1,
            1,
            None,
            None,
            None,
            None,
            Some("FLAC"),
            None,
            None,
            None,
            None,
            None,
            None,
            None,
            None,
            false,
            None,
            None,
            None,
            None,
            None,
            None,
            [1; 32],
        )
        .await
        .expect("stage Track");
        let ScanOutcome::Changed(publication) = scan.finish().await.expect("publish Scan") else {
            panic!("initial Scan must publish")
        };
        let cancellation = ReadCancellation::new();
        let track_key = database
            .track_order(
                publication.source,
                None,
                false,
                TrackSort::Title,
                false,
                &cancellation,
            )
            .await
            .expect("Track order")[0];
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
                user_token: "token".to_string(),
                now_playing_enabled: true,
            },
            ..Settings::default()
        };
        let scrobbler = tokio::task::block_in_place(|| {
            Scrobbler::new(
                database.clone(),
                tokio::runtime::Handle::current(),
                settings,
                false,
            )
        })
        .expect("start Scrobbler");
        let track = ListeningTrack {
            source_key: publication.source,
            track_key: Some(track_key),
            track_object_id: "track".to_string(),
            recording_id: None,
            title: "Track".to_string(),
            artists: vec!["Artist".to_string()],
            album: Some("Album".to_string()),
            track_number: Some(1),
            disc_number: Some(1),
            duration_millis: 180_000,
        };
        let deliveries = scrobbler.listen_delivery_targets(&track);
        assert_eq!(deliveries.len(), 2);
        let listen = ListenWrite {
            external_id: "play".to_string(),
            track_key: track.track_key,
            track_object_id: track.track_object_id.clone(),
            track_title: track.title.clone(),
            artist_name: track.artists.join(", "),
            album_title: track.album.clone().unwrap_or_default(),
            started_at: 100,
            local_period: "1970-01".to_string(),
            duration_millis: 180_000,
            listened_millis: 90_000,
            skipped: false,
        };
        database
            .record_listen(publication.source, &listen, &deliveries)
            .await
            .expect("record Activity");
        database
            .record_listen(publication.source, &listen, &deliveries)
            .await
            .expect("record Activity idempotently");

        assert_eq!(
            database
                .activity_history(publication.source, &cancellation)
                .await
                .expect("Activity")
                .len(),
            1
        );
        let due = database
            .due_listen_deliveries(i64::MAX, 10, &cancellation)
            .await
            .expect("delivery outbox");
        assert_eq!(due.len(), 2);
        let retry_track = SubmissionTrack::from_pending(&due[0]);
        assert_eq!(retry_track.duration_millis, 180_000);
        assert_eq!(due[0].listened_millis, 90_000);
        let lastfm_account = opaque_account_id(DeliveryService::LastFm, "listener");
        assert_eq!(
            database
                .block_listen_deliveries("lastfm", &lastfm_account, "invalid session")
                .await
                .expect("block account"),
            1
        );
        assert_eq!(
            database
                .due_listen_deliveries(i64::MAX, 10, &cancellation)
                .await
                .expect("blocked outbox")
                .len(),
            1
        );
        assert_eq!(
            database
                .wake_listen_deliveries("lastfm", &lastfm_account, 101)
                .await
                .expect("wake account"),
            1
        );
        assert_eq!(
            database
                .remove_listen_deliveries("lastfm", &lastfm_account)
                .await
                .expect("remove account"),
            1
        );
        assert_eq!(
            database
                .activity_history(publication.source, &cancellation)
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
