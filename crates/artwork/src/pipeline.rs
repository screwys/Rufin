use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet, VecDeque};
use std::path::Path;
use std::sync::mpsc::{self, RecvTimeoutError};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, Weak};
use std::thread;
use std::time::Duration;

use sources::SourceId;
use tokio::runtime::Handle;
use tokio::sync::oneshot;
use tracing::warn;

use crate::cache::FilesystemCache;
use crate::decode::{decode_cached, decode_normalized, normalize_for_cache};
use crate::fetch::{FetchContext, FetchOutcome};
use crate::selection::Candidate;
use crate::{
    ArtworkBinding, ArtworkBindingIdentity, ArtworkError, ArtworkKey, ArtworkLoad, ArtworkOutcome,
    ArtworkPreparation, ArtworkRequest, ArtworkRequestIdentity, ArtworkVisualIdentity,
    DecodedImage, ExternalPolicy, PendingArtwork, RequestId, SourceImages,
};

pub(crate) const WORKERS: usize = 4;
pub(crate) const PREPARATION_WORKERS: usize = WORKERS - 1;
// Keep every worker fed without mirroring the selected source in the job table.
pub(crate) const PREPARATION_WINDOW: usize = PREPARATION_WORKERS * 4;
const MAX_DECODED_INDEX_ENTRIES: usize = 4_096;
const SOURCE_ARTWORK_SIZE: u32 = 256;

pub(crate) struct Pipeline {
    shared: Arc<Shared>,
}

struct Shared {
    runtime: Handle,
    cache: FilesystemCache,
    fetch: FetchContext,
    cache_commit: Mutex<()>,
    state: Mutex<State>,
    wake: Condvar,
}

#[derive(Default)]
struct State {
    next_request: u64,
    next_preparation: u64,
    external_epoch: u64,
    source_epochs: HashMap<SourceId, u64>,
    foreground: VecDeque<JobKey>,
    preparations: VecDeque<JobKey>,
    jobs: HashMap<JobKey, JobRecord>,
    projections: HashMap<RequestId, ProjectionRecord>,
    decoded_index: DecodedIndex,
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
struct JobKey(String);

struct JobRecord {
    source: SourceImages,
    request: CandidateRequest,
    subscribers: HashSet<RequestId>,
    foreground_subscribers: HashSet<RequestId>,
    preparations: Vec<PreparationSubscriber>,
    active: bool,
    source_epoch: u64,
    external_epoch: u64,
}

#[derive(Clone)]
struct Work {
    key: JobKey,
    source: SourceImages,
    request: CandidateRequest,
    source_epoch: u64,
    external_epoch: u64,
    decode: bool,
}

#[derive(Clone)]
struct CandidateRequest {
    candidate: Candidate,
    fetch_size: u32,
    render_size: u32,
    external: ExternalPolicy,
}

struct PreparationSubscriber {
    id: u64,
    completion: mpsc::Sender<BackgroundResult>,
}

#[derive(Clone, Copy)]
enum BackgroundResult {
    Ready,
    Cached,
    Missing,
    Failed,
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
enum JobPriority {
    Preparation,
    Foreground,
}

struct ProjectionRecord {
    source: SourceImages,
    request: ArtworkRequest,
    priority: JobPriority,
    candidate_index: usize,
    failures: Vec<Arc<str>>,
    job: JobKey,
    completion: oneshot::Sender<ArtworkOutcome>,
}

#[derive(Default)]
struct DecodedIndex {
    entries: HashMap<ArtworkKey, DecodedEntry>,
    families: HashMap<DecodedFamily, BTreeMap<u32, HashSet<ArtworkKey>>>,
    eviction_order: BTreeSet<DecodedAccess>,
    next_access: u64,
}

struct DecodedEntry {
    source_id: SourceId,
    family: DecodedFamily,
    render_size: u32,
    image: Weak<DecodedImage>,
    last_used: u64,
}

#[derive(Clone, Eq, Ord, PartialEq, PartialOrd)]
struct DecodedAccess {
    last_used: u64,
    key: ArtworkKey,
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
struct DecodedFamily(String);

enum Resolution {
    Ready {
        image: Arc<DecodedImage>,
        family: DecodedFamily,
    },
    Cached,
    Missing,
    Failed(Arc<str>),
}

type LeaseCompletion = (oneshot::Sender<ArtworkOutcome>, ArtworkOutcome);

impl Pipeline {
    pub(crate) fn begin_source_manifest(
        &self,
        source_id: &SourceId,
        revision: u64,
    ) -> std::io::Result<std::path::PathBuf> {
        self.shared.cache.begin_source_manifest(source_id, revision)
    }

    pub(crate) fn mark_source_manifest(
        &self,
        staging: &std::path::Path,
        bindings: &[Vec<u8>],
    ) -> std::io::Result<()> {
        for binding in bindings {
            if let Some(candidate) = ArtworkBinding::opaque(binding).candidates().first() {
                self.shared
                    .cache
                    .mark_source_manifest_identity(staging, &candidate.stable_identity())?;
            }
        }
        Ok(())
    }

    pub(crate) fn complete_source_manifest(
        &self,
        source_id: &SourceId,
        revision: u64,
        staging: &std::path::Path,
    ) -> std::io::Result<()> {
        let _commit = lock_cache_commit(&self.shared);
        self.shared
            .cache
            .complete_source_manifest_staging(source_id, revision, staging)
    }
    pub(crate) fn new(cache_root: &Path, runtime: Handle) -> Result<Self, ArtworkError> {
        let cache = FilesystemCache::new(cache_root.to_path_buf())?;
        let fetch = FetchContext::new().map_err(ArtworkError::FetchSetup)?;
        let shared = Arc::new(Shared {
            runtime,
            cache,
            fetch,
            cache_commit: Mutex::new(()),
            state: Mutex::new(State {
                next_request: 1,
                next_preparation: 1,
                ..State::default()
            }),
            wake: Condvar::new(),
        });
        for index in 0..WORKERS {
            let worker = Arc::clone(&shared);
            thread::Builder::new()
                .name(format!("artwork-{index}"))
                .spawn(move || run_worker(worker, index == 0))
                .map_err(ArtworkError::Cache)?;
        }
        Ok(Self { shared })
    }

    pub(crate) fn request(
        self: &Arc<Self>,
        source: SourceImages,
        request: ArtworkRequest,
    ) -> Result<ArtworkLoad, ArtworkError> {
        self.request_with_priority(source, request, JobPriority::Foreground)
    }

    fn request_with_priority(
        self: &Arc<Self>,
        source: SourceImages,
        request: ArtworkRequest,
        priority: JobPriority,
    ) -> Result<ArtworkLoad, ArtworkError> {
        let mut state = lock_state(&self.shared);
        let request_id = RequestId(state.next_request);
        state.next_request = state.next_request.wrapping_add(1).max(1);
        let ready = decoded_from_memory(&mut state, &source, &request);
        if let Some(image) = ready {
            return Ok(ArtworkLoad::Ready(image));
        }
        let Some(candidate) = request.binding.candidates().first().cloned() else {
            return Ok(ArtworkLoad::Missing);
        };
        let (completion, receiver) = oneshot::channel();
        let job = enqueue_projection(
            &mut state,
            source.clone(),
            candidate_request(&request, candidate),
            request_id,
            priority,
        );
        state.projections.insert(
            request_id,
            ProjectionRecord {
                source,
                request,
                priority,
                candidate_index: 0,
                failures: Vec::new(),
                job,
                completion,
            },
        );
        drop(state);
        self.shared.wake.notify_one();
        Ok(ArtworkLoad::Pending(PendingArtwork {
            request_id,
            completion: Some(receiver),
            pipeline: Arc::clone(self),
        }))
    }

    pub(crate) fn source_preparation_complete(
        &self,
        source_id: &SourceId,
        revision: u64,
    ) -> Result<bool, ArtworkError> {
        self.shared
            .cache
            .source_manifest_complete(source_id, revision)
            .map_err(ArtworkError::Cache)
    }

    pub(crate) fn prefetch_source_artwork(
        &self,
        source: SourceImages,
        artwork: Arc<[Vec<u8>]>,
        progress: &(dyn Fn(usize, usize) + Send + Sync),
        cancelled: &(dyn Fn() -> bool + Send + Sync),
    ) -> Result<ArtworkPreparation, ArtworkError> {
        self.prepare_source_artwork_jobs(source, artwork, progress, cancelled)
    }

    fn prepare_source_artwork_jobs(
        &self,
        source: SourceImages,
        artwork: Arc<[Vec<u8>]>,
        progress: &(dyn Fn(usize, usize) + Send + Sync),
        cancelled: &(dyn Fn() -> bool + Send + Sync),
    ) -> Result<ArtworkPreparation, ArtworkError> {
        let total = artwork.len();
        if total == 0 {
            return Ok(ArtworkPreparation::default());
        }
        if cancelled() {
            return Err(ArtworkError::Cancelled);
        }
        let (completion, completed) = mpsc::channel();
        let preparation_id = {
            let mut state = lock_state(&self.shared);
            let id = state.next_preparation;
            state.next_preparation = state.next_preparation.wrapping_add(1).max(1);
            id
        };

        let mut summary = ArtworkPreparation {
            total,
            ..ArtworkPreparation::default()
        };
        let mut admitted = 0;
        let mut completed_count = 0;
        while completed_count < total {
            if cancelled() {
                cancel_preparation(&self.shared, preparation_id);
                return Err(ArtworkError::Cancelled);
            }

            let in_flight = admitted - completed_count;
            let end = total.min(admitted + PREPARATION_WINDOW.saturating_sub(in_flight));
            if admitted < end {
                let mut state = lock_state(&self.shared);
                for source_artwork in &artwork[admitted..end] {
                    let request = ArtworkRequest::new(
                        ArtworkBinding::opaque(source_artwork),
                        SOURCE_ARTWORK_SIZE,
                        SOURCE_ARTWORK_SIZE,
                    );
                    if request.binding.candidates().is_empty() {
                        let _ = completion.send(BackgroundResult::Missing);
                        continue;
                    }
                    if decoded_for_request(&mut state, &self.shared.cache, &source, &request)
                        .is_some()
                    {
                        let _ = completion.send(BackgroundResult::Cached);
                        continue;
                    }
                    enqueue_background(
                        &mut state,
                        source.clone(),
                        request,
                        PreparationSubscriber {
                            id: preparation_id,
                            completion: completion.clone(),
                        },
                    );
                }
                admitted = end;
                drop(state);
                self.shared.wake.notify_all();
            }

            match completed.recv_timeout(Duration::from_millis(25)) {
                Ok(BackgroundResult::Ready) => summary.ready += 1,
                Ok(BackgroundResult::Cached) => {
                    summary.ready += 1;
                    summary.cached += 1;
                }
                Ok(BackgroundResult::Missing) => summary.missing += 1,
                Ok(BackgroundResult::Failed) => summary.failed += 1,
                Err(RecvTimeoutError::Timeout) => continue,
                Err(RecvTimeoutError::Disconnected) => {
                    cancel_preparation(&self.shared, preparation_id);
                    return Err(ArtworkError::Cancelled);
                }
            }
            completed_count += 1;
            progress(completed_count, total);
        }
        Ok(summary)
    }

    pub(crate) fn cancel(&self, request_id: RequestId) {
        let mut state = lock_state(&self.shared);
        let Some(projection) = state.projections.remove(&request_id) else {
            return;
        };
        if let Some(record) = state.jobs.get_mut(&projection.job) {
            record.subscribers.remove(&request_id);
            record.foreground_subscribers.remove(&request_id);
        }
        reschedule_or_remove(&mut state, &projection.job, false);
        drop(state);
        self.shared.wake.notify_all();
    }

    pub(crate) fn cache_only_file(
        &self,
        source_id: &SourceId,
        request: &ArtworkRequest,
    ) -> Option<std::path::PathBuf> {
        for candidate in request.binding.candidates() {
            if candidate.is_external() && !request.external.allow_cached {
                continue;
            }
            if let Some(entry) =
                self.shared
                    .cache
                    .ready_entry(source_id, candidate, request.fetch_size)
            {
                return Some(entry.path);
            }
        }
        None
    }

    pub(crate) fn binding_identity_and_image(
        &self,
        source: &SourceImages,
        request: &ArtworkRequest,
    ) -> (ArtworkBindingIdentity, Option<Arc<DecodedImage>>) {
        let mut state = lock_state(&self.shared);
        binding_identity_and_image_from_state(&mut state, source, request)
    }

    pub(crate) fn retry_external(&self) -> Result<(), ArtworkError> {
        let commit = lock_cache_commit(&self.shared);
        self.shared.cache.retry_external()?;
        let mut state = lock_state(&self.shared);
        state.external_epoch = state.external_epoch.wrapping_add(1);
        let completions = reconcile_inactive_external_jobs(&mut state);
        drop(state);
        drop(commit);
        send_completions(completions);
        self.shared.wake.notify_all();
        Ok(())
    }

    pub(crate) fn invalidate_source(&self, source_id: &SourceId) -> Result<(), ArtworkError> {
        let commit = lock_cache_commit(&self.shared);
        self.shared.cache.invalidate_source(source_id)?;
        let mut state = lock_state(&self.shared);
        *state.source_epochs.entry(source_id.clone()).or_default() = state
            .source_epochs
            .get(source_id)
            .copied()
            .unwrap_or_default()
            .wrapping_add(1);
        state.decoded_index.invalidate_source(source_id);
        let invalidated = state
            .projections
            .iter()
            .filter(|(_, record)| record.source.source_id == *source_id)
            .map(|(request_id, _)| *request_id)
            .collect::<HashSet<_>>();
        let completions = invalidated
            .iter()
            .filter_map(|request_id| state.projections.remove(request_id))
            .map(|record| record.completion)
            .collect::<Vec<_>>();
        for record in state.jobs.values_mut() {
            if record.source.source_id == *source_id {
                record
                    .subscribers
                    .retain(|request_id| !invalidated.contains(request_id));
                record
                    .foreground_subscribers
                    .retain(|request_id| !invalidated.contains(request_id));
            }
        }
        let removable = state
            .jobs
            .iter()
            .filter(|(_, record)| record.source.source_id == *source_id && !record.active)
            .map(|(key, _)| key.clone())
            .collect::<Vec<_>>();
        for key in &removable {
            remove_job(&mut state, key);
        }
        drop(state);
        drop(commit);
        for completion in completions {
            let _ = completion.send(ArtworkOutcome::Invalidated);
        }
        Ok(())
    }
}

fn binding_identity_from_state(
    state: &State,
    source: &SourceImages,
    request: &ArtworkRequest,
) -> ArtworkBindingIdentity {
    let source_epoch = source_epoch(state, &source.source_id);
    let external_epoch = request
        .binding
        .has_external()
        .then_some(state.external_epoch)
        .unwrap_or_default();
    let visual = artwork_visual_identity(source, request, source_epoch);
    let request = request_identity(source, request, source_epoch, external_epoch);
    ArtworkBindingIdentity {
        visual,
        request: ArtworkRequestIdentity::new(request),
    }
}

fn binding_identity_and_image_from_state(
    state: &mut State,
    source: &SourceImages,
    request: &ArtworkRequest,
) -> (ArtworkBindingIdentity, Option<Arc<DecodedImage>>) {
    let identity = binding_identity_from_state(state, source, request);
    let ready = decoded_from_memory(state, source, request);
    (identity, ready)
}

fn decoded_from_memory(
    state: &mut State,
    source: &SourceImages,
    request: &ArtworkRequest,
) -> Option<Arc<DecodedImage>> {
    let source_epoch = source_epoch(state, &source.source_id);
    let candidate = request.binding.candidates().first()?;
    let external = candidate.is_external();
    if external && !request.external.allow_cached && !request.external.allow_network {
        return None;
    }
    let external_epoch = if external { state.external_epoch } else { 0 };
    let family =
        decoded_candidate_family(&source.source_id, candidate, source_epoch, external_epoch);
    let exact = decoded_key(&family, request.fetch_size, request.render_size);
    state
        .decoded_index
        .get_for_request(&exact, &family, request.render_size)
}

fn artwork_visual_identity(
    source: &SourceImages,
    request: &ArtworkRequest,
    source_epoch: u64,
) -> ArtworkVisualIdentity {
    let cached_external = request.binding.has_external() && request.external.allow_cached;
    ArtworkVisualIdentity::new(format!(
        "{}\0{}\0{}\0{}",
        source.source_id,
        request.binding.stable_identity(),
        cached_external,
        source_epoch,
    ))
}

fn decoded_for_request(
    state: &mut State,
    cache: &FilesystemCache,
    source: &SourceImages,
    request: &ArtworkRequest,
) -> Option<Arc<DecodedImage>> {
    let source_epoch = source_epoch(state, &source.source_id);
    for candidate in request.binding.candidates() {
        let external = candidate.is_external();
        let may_read_cache = !external || request.external.allow_cached;
        let external_epoch = if external { state.external_epoch } else { 0 };
        let family =
            decoded_candidate_family(&source.source_id, candidate, source_epoch, external_epoch);
        let exact = decoded_key(&family, request.fetch_size, request.render_size);
        if may_read_cache {
            if let Some(image) =
                state
                    .decoded_index
                    .get_for_request(&exact, &family, request.render_size)
            {
                return Some(image);
            }
            if cache
                .ready_entry(&source.source_id, candidate, request.fetch_size)
                .is_some()
            {
                return None;
            }
            if cache.is_missing(&source.source_id, candidate, request.fetch_size) {
                continue;
            }
        }
        if external && !request.external.allow_network {
            continue;
        }
        if source.can_fetch() {
            return None;
        }
    }
    None
}

fn decoded_candidate_family(
    source_id: &SourceId,
    candidate: &Candidate,
    source_epoch: u64,
    external_epoch: u64,
) -> DecodedFamily {
    DecodedFamily(format!(
        "{}\0{}\0{}\0{}",
        source_id,
        candidate.stable_identity(),
        source_epoch,
        external_epoch
    ))
}

fn decoded_key(family: &DecodedFamily, fetch_size: u32, render_size: u32) -> ArtworkKey {
    ArtworkKey::new(format!("{}\0{fetch_size}\0{render_size}", family.0))
}

impl DecodedIndex {
    fn get(&mut self, key: &ArtworkKey) -> Option<Arc<DecodedImage>> {
        let image = self.entries.get(key)?.image.upgrade();
        let Some(image) = image else {
            self.remove_entry(key);
            return None;
        };
        let last_used = self.next_access();
        let previous_access = {
            let entry = self.entries.get_mut(key)?;
            let previous_access = DecodedAccess {
                last_used: entry.last_used,
                key: key.clone(),
            };
            entry.last_used = last_used;
            previous_access
        };
        self.eviction_order.remove(&previous_access);
        self.eviction_order.insert(DecodedAccess {
            last_used,
            key: key.clone(),
        });
        Some(image)
    }

    fn get_for_request(
        &mut self,
        exact_key: &ArtworkKey,
        family: &DecodedFamily,
        render_size: u32,
    ) -> Option<Arc<DecodedImage>> {
        if let Some(image) = self.get(exact_key) {
            return Some(image);
        }
        let reusable = self
            .families
            .get(family)
            .into_iter()
            .flat_map(|sizes| sizes.range(render_size..))
            .flat_map(|(_, keys)| keys.iter().cloned())
            .collect::<Vec<_>>();
        reusable
            .into_iter()
            .find_map(|reusable| self.get(&reusable))
    }

    fn insert(
        &mut self,
        key: ArtworkKey,
        source_id: SourceId,
        family: DecodedFamily,
        render_size: u32,
        image: Arc<DecodedImage>,
    ) {
        self.insert_with_limit(
            key,
            source_id,
            family,
            render_size,
            image,
            MAX_DECODED_INDEX_ENTRIES,
        );
    }

    fn insert_with_limit(
        &mut self,
        key: ArtworkKey,
        source_id: SourceId,
        family: DecodedFamily,
        render_size: u32,
        image: Arc<DecodedImage>,
        max_entries: usize,
    ) {
        self.remove_entry(&key);
        let last_used = self.next_access();
        self.families
            .entry(family.clone())
            .or_default()
            .entry(render_size)
            .or_default()
            .insert(key.clone());
        self.entries.insert(
            key.clone(),
            DecodedEntry {
                source_id,
                family,
                render_size,
                image: Arc::downgrade(&image),
                last_used,
            },
        );
        self.eviction_order.insert(DecodedAccess { last_used, key });
        self.evict_to_limit(max_entries);
    }

    fn invalidate_source(&mut self, source_id: &SourceId) {
        let stale = self
            .entries
            .iter()
            .filter(|(_, entry)| entry.source_id == *source_id)
            .map(|(key, _)| key.clone())
            .collect::<Vec<_>>();
        for key in stale {
            self.remove_entry(&key);
        }
    }

    fn remove_entry(&mut self, key: &ArtworkKey) -> Option<DecodedEntry> {
        let removed = self.entries.remove(key)?;
        self.eviction_order.remove(&DecodedAccess {
            last_used: removed.last_used,
            key: key.clone(),
        });
        let remove_family = self.families.get_mut(&removed.family).is_some_and(|sizes| {
            if let Some(keys) = sizes.get_mut(&removed.render_size) {
                keys.remove(key);
                if keys.is_empty() {
                    sizes.remove(&removed.render_size);
                }
            }
            sizes.is_empty()
        });
        if remove_family {
            self.families.remove(&removed.family);
        }
        Some(removed)
    }

    fn next_access(&mut self) -> u64 {
        self.next_access = self.next_access.wrapping_add(1).max(1);
        self.next_access
    }

    fn evict_to_limit(&mut self, max_entries: usize) {
        while self.entries.len() > max_entries {
            let Some(access) = self.eviction_order.first().cloned() else {
                break;
            };
            self.remove_entry(&access.key);
        }
    }
}

fn reconcile_inactive_external_jobs(state: &mut State) -> Vec<LeaseCompletion> {
    let stale = state
        .jobs
        .iter()
        .filter(|(_, record)| {
            !record.active
                && record.request.candidate.is_external()
                && record.external_epoch != state.external_epoch
        })
        .map(|(key, _)| key.clone())
        .collect::<Vec<_>>();
    let stale = stale
        .iter()
        .filter_map(|key| remove_job(state, key))
        .collect::<Vec<_>>();

    let mut completions = Vec::new();
    for record in stale {
        for request_id in record.subscribers {
            if let Some(completion) = restart_projection(state, request_id) {
                completions.push(completion);
            }
        }
        for subscriber in record.preparations {
            enqueue_background_candidate(
                state,
                record.source.clone(),
                record.request.clone(),
                subscriber,
            );
        }
    }
    completions
}

impl JobRecord {
    fn has_interest(&self) -> bool {
        !self.subscribers.is_empty() || !self.preparations.is_empty()
    }

    fn priority(&self) -> JobPriority {
        if !self.foreground_subscribers.is_empty() {
            JobPriority::Foreground
        } else {
            JobPriority::Preparation
        }
    }
}

fn enqueue_projection(
    state: &mut State,
    source: SourceImages,
    request: CandidateRequest,
    subscriber: RequestId,
    priority: JobPriority,
) -> JobKey {
    let source_epoch = source_epoch(state, &source.source_id);
    let external_epoch = request
        .candidate
        .is_external()
        .then_some(state.external_epoch)
        .unwrap_or_default();
    let key = job_key(&source, &request, source_epoch, external_epoch);
    if let Some(record) = state.jobs.get_mut(&key) {
        if source.can_fetch() && !record.source.can_fetch() {
            record.source = source;
        }
        record.subscribers.insert(subscriber);
        if matches!(priority, JobPriority::Foreground) {
            record.foreground_subscribers.insert(subscriber);
        }
        let active = record.active;
        if !active && matches!(priority, JobPriority::Foreground) {
            queue(state, key.clone(), true);
        }
        return key;
    }
    let subscribers = HashSet::from([subscriber]);
    let foreground_subscribers = matches!(priority, JobPriority::Foreground)
        .then(|| HashSet::from([subscriber]))
        .unwrap_or_default();
    state.jobs.insert(
        key.clone(),
        JobRecord {
            source,
            request,
            subscribers,
            foreground_subscribers,
            preparations: Vec::new(),
            active: false,
            source_epoch,
            external_epoch,
        },
    );
    queue(state, key.clone(), false);
    key
}

fn enqueue_background(
    state: &mut State,
    source: SourceImages,
    request: ArtworkRequest,
    subscriber: PreparationSubscriber,
) -> Option<JobKey> {
    let candidate = request.binding.candidates().first()?.clone();
    Some(enqueue_background_candidate(
        state,
        source,
        candidate_request(&request, candidate),
        subscriber,
    ))
}

fn enqueue_background_candidate(
    state: &mut State,
    source: SourceImages,
    request: CandidateRequest,
    subscriber: PreparationSubscriber,
) -> JobKey {
    let source_epoch = source_epoch(state, &source.source_id);
    let external_epoch = request
        .candidate
        .is_external()
        .then_some(state.external_epoch)
        .unwrap_or_default();
    let key = job_key(&source, &request, source_epoch, external_epoch);
    if let Some(record) = state.jobs.get_mut(&key) {
        if source.can_fetch() && !record.source.can_fetch() {
            record.source = source;
        }
        record.preparations.push(subscriber);
        if !record.active {
            queue(state, key.clone(), false);
        }
        return key;
    }
    state.jobs.insert(
        key.clone(),
        JobRecord {
            source,
            request,
            subscribers: HashSet::new(),
            foreground_subscribers: HashSet::new(),
            preparations: vec![subscriber],
            active: false,
            source_epoch,
            external_epoch,
        },
    );
    queue(state, key.clone(), false);
    key
}

fn cancel_preparation(shared: &Shared, id: u64) {
    let mut state = lock_state(shared);
    let keys = state.jobs.keys().cloned().collect::<Vec<_>>();
    for key in keys {
        if let Some(record) = state.jobs.get_mut(&key) {
            record.preparations.retain(|subscriber| subscriber.id != id);
        }
        reschedule_or_remove(&mut state, &key, false);
    }
    drop(state);
    shared.wake.notify_all();
}

fn reschedule_or_remove(state: &mut State, key: &JobKey, front: bool) {
    let Some((active, has_interest)) = state
        .jobs
        .get(key)
        .map(|record| (record.active, record.has_interest()))
    else {
        remove_queued(state, key);
        return;
    };
    if active {
        remove_queued(state, key);
    } else if has_interest {
        queue(state, key.clone(), front);
    } else {
        remove_job(state, key);
    }
}

fn remove_job(state: &mut State, key: &JobKey) -> Option<JobRecord> {
    remove_queued(state, key);
    state.jobs.remove(key)
}

fn queue(state: &mut State, key: JobKey, front: bool) {
    remove_queued(state, &key);
    let Some(priority) = state.jobs.get(&key).map(JobRecord::priority) else {
        return;
    };
    let queue = match priority {
        JobPriority::Foreground => &mut state.foreground,
        JobPriority::Preparation => &mut state.preparations,
    };
    if front {
        queue.push_front(key);
    } else {
        queue.push_back(key);
    }
}

fn remove_queued(state: &mut State, key: &JobKey) {
    state.foreground.retain(|queued| queued != key);
    state.preparations.retain(|queued| queued != key);
}

fn job_key(
    source: &SourceImages,
    request: &CandidateRequest,
    source_epoch: u64,
    external_epoch: u64,
) -> JobKey {
    let identity = format!(
        "{}\0{}\0{}\0{}\0{}\0{}\0{}\0{}\0{:x}",
        source.source_id,
        request.candidate.stable_identity(),
        request.fetch_size,
        request.render_size,
        request.external.allow_cached,
        request.external.allow_network,
        request.external.allow_musicbrainz,
        source_epoch,
        md5::compute(request.external.lastfm_api_key.as_bytes())
    );
    JobKey(format!(
        "{:x}-{external_epoch}",
        md5::compute(identity.as_bytes())
    ))
}

fn candidate_request(request: &ArtworkRequest, candidate: Candidate) -> CandidateRequest {
    CandidateRequest {
        candidate,
        fetch_size: request.fetch_size,
        render_size: request.render_size,
        external: request.external.clone(),
    }
}

fn request_identity(
    source: &SourceImages,
    request: &ArtworkRequest,
    source_epoch: u64,
    external_epoch: u64,
) -> String {
    let identity = format!(
        "{}\0{}\0{}\0{}\0{}\0{}\0{}\0{}\0{}\0{:x}",
        source.source_id,
        request.binding.stable_identity(),
        request.fetch_size,
        request.render_size,
        request.external.allow_cached,
        request.external.allow_network,
        request.external.allow_musicbrainz,
        source.can_fetch(),
        source_epoch,
        md5::compute(request.external.lastfm_api_key.as_bytes())
    );
    format!("{:x}-{external_epoch}", md5::compute(identity.as_bytes()))
}

fn source_epoch(state: &State, source_id: &SourceId) -> u64 {
    state
        .source_epochs
        .get(source_id)
        .copied()
        .unwrap_or_default()
}

fn run_worker(shared: Arc<Shared>, foreground_reserved: bool) {
    loop {
        let work = next_work(&shared, foreground_reserved);
        let resolution = resolve(&shared, &work);
        finish(&shared, work, resolution);
    }
}

fn next_work(shared: &Shared, foreground_reserved: bool) -> Work {
    let mut state = lock_state(shared);
    loop {
        let key = state.foreground.pop_front().or_else(|| {
            (!foreground_reserved)
                .then(|| state.preparations.pop_front())
                .flatten()
        });
        if let Some(key) = key {
            let eligible = state
                .jobs
                .get(&key)
                .is_some_and(|record| !record.active && record.has_interest());
            if !eligible {
                reschedule_or_remove(&mut state, &key, false);
                continue;
            }
            let record = state.jobs.get_mut(&key).expect("eligible artwork job");
            record.active = true;
            let work = Work {
                key,
                source: record.source.clone(),
                request: record.request.clone(),
                source_epoch: record.source_epoch,
                external_epoch: record.external_epoch,
                decode: !record.subscribers.is_empty(),
            };
            return work;
        }
        state = shared
            .wake
            .wait(state)
            .unwrap_or_else(std::sync::PoisonError::into_inner);
    }
}

fn resolve(shared: &Shared, work: &Work) -> Resolution {
    resolve_candidate(shared, work)
}

fn resolve_candidate(shared: &Shared, work: &Work) -> Resolution {
    let source = &work.source;
    let request = &work.request;
    let candidate = &request.candidate;
    let mut failures = Vec::new();
    let external = candidate.is_external();
    let family = decoded_candidate_family(
        &source.source_id,
        candidate,
        work.source_epoch,
        if external { work.external_epoch } else { 0 },
    );
    let artwork_key = decoded_key(&family, request.fetch_size, request.render_size);
    let may_read_cache = !external || request.external.allow_cached;
    if may_read_cache {
        if let Some(entry) =
            shared
                .cache
                .ready_entry(&source.source_id, candidate, request.fetch_size)
        {
            if !work.decode {
                return Resolution::Cached;
            }
            match decode_cached(entry.path.clone(), artwork_key.clone(), request.render_size) {
                Ok(image) => {
                    return Resolution::Ready {
                        image: Arc::new(image),
                        family,
                    };
                }
                Err(error) => {
                    shared.cache.remove_ready(&entry.path);
                    failures.push(error.to_string());
                }
            }
        }
        if shared
            .cache
            .is_missing(&source.source_id, candidate, request.fetch_size)
        {
            return Resolution::Missing;
        }
    }
    if external && !request.external.allow_network {
        return Resolution::Missing;
    }
    if !external && !source.can_fetch() {
        return Resolution::Missing;
    }
    match shared.fetch.fetch(
        &shared.runtime,
        source,
        candidate,
        request.fetch_size,
        &request.external,
    ) {
        Ok(FetchOutcome::Ready(bytes)) => {
            let normalized = match normalize_for_cache(bytes, request.fetch_size) {
                Ok(normalized) => normalized,
                Err(error) => return Resolution::Failed(error.to_string().into()),
            };
            match write_ready(shared, work, normalized.bytes()) {
                Ok(Some(_path)) if !work.decode => Resolution::Cached,
                Ok(Some(path)) => match decode_normalized(
                    normalized,
                    path.clone(),
                    artwork_key,
                    request.render_size,
                ) {
                    Ok(image) => Resolution::Ready {
                        image: Arc::new(image),
                        family,
                    },
                    Err(error) => {
                        shared.cache.remove_ready(&path);
                        failures.push(error.to_string());
                        Resolution::Failed(failures.join("; ").into())
                    }
                },
                Ok(None) => Resolution::Missing,
                Err(error) => {
                    failures.push(error.to_string());
                    Resolution::Failed(failures.join("; ").into())
                }
            }
        }
        Ok(FetchOutcome::Missing) => match mark_missing(shared, work) {
            Ok(true) => Resolution::Missing,
            Ok(false) => Resolution::Missing,
            Err(error) => {
                failures.push(error.to_string());
                Resolution::Failed(failures.join("; ").into())
            }
        },
        Err(error) => {
            failures.push(error);
            Resolution::Failed(failures.join("; ").into())
        }
    }
}

fn write_ready(
    shared: &Shared,
    work: &Work,
    bytes: &[u8],
) -> std::io::Result<Option<std::path::PathBuf>> {
    let _commit = lock_cache_commit(shared);
    let state = lock_state(shared);
    if !work_is_current(&state, work) {
        return Ok(None);
    }
    drop(state);
    shared
        .cache
        .write_ready(
            &work.source.source_id,
            &work.request.candidate,
            work.request.fetch_size,
            bytes,
        )
        .map(Some)
}

fn mark_missing(shared: &Shared, work: &Work) -> std::io::Result<bool> {
    let _commit = lock_cache_commit(shared);
    let state = lock_state(shared);
    if !work_is_current(&state, work) {
        return Ok(false);
    }
    drop(state);
    shared.cache.mark_missing(
        &work.source.source_id,
        &work.request.candidate,
        work.request.fetch_size,
    )?;
    Ok(true)
}

fn work_is_current(state: &State, work: &Work) -> bool {
    source_epoch(state, &work.source.source_id) == work.source_epoch
        && (!work.request.candidate.is_external() || state.external_epoch == work.external_epoch)
}

fn finish(shared: &Shared, work: Work, resolution: Resolution) {
    let mut state = lock_state(shared);
    let Some(record) = remove_job(&mut state, &work.key) else {
        drop(state);
        shared.wake.notify_all();
        return;
    };
    if source_epoch(&state, &work.source.source_id) != work.source_epoch {
        drop(state);
        shared.wake.notify_all();
        return;
    }
    if work.request.candidate.is_external() && state.external_epoch != work.external_epoch {
        let mut completions = Vec::new();
        for request_id in record.subscribers {
            if let Some(completion) = restart_projection(&mut state, request_id) {
                completions.push(completion);
            }
        }
        for subscriber in record.preparations {
            enqueue_background_candidate(
                &mut state,
                record.source.clone(),
                record.request.clone(),
                subscriber,
            );
        }
        drop(state);
        send_completions(completions);
        shared.wake.notify_all();
        return;
    }
    if !work.source.can_fetch()
        && record.source.can_fetch()
        && matches!(&resolution, Resolution::Missing | Resolution::Failed(_))
    {
        for request_id in record.subscribers {
            let priority = state
                .projections
                .get(&request_id)
                .map(|projection| projection.priority)
                .unwrap_or(JobPriority::Preparation);
            let job = enqueue_projection(
                &mut state,
                record.source.clone(),
                record.request.clone(),
                request_id,
                priority,
            );
            if let Some(projection) = state.projections.get_mut(&request_id) {
                projection.job = job;
            }
        }
        for subscriber in record.preparations {
            enqueue_background_candidate(
                &mut state,
                record.source.clone(),
                record.request.clone(),
                subscriber,
            );
        }
        drop(state);
        shared.wake.notify_all();
        return;
    }
    if record.has_interest()
        && let Resolution::Ready { image, family } = &resolution
    {
        state.decoded_index.insert(
            image.key().clone(),
            work.source.source_id.clone(),
            family.clone(),
            work.request.render_size,
            Arc::clone(image),
        );
    }
    let mut completions = Vec::new();
    for request_id in record.subscribers {
        match &resolution {
            Resolution::Ready { image, .. } => {
                let Some(projection) = state.projections.remove(&request_id) else {
                    continue;
                };
                completions.push((
                    projection.completion,
                    ArtworkOutcome::Ready(Arc::clone(image)),
                ));
            }
            Resolution::Cached => {
                let candidate_index = state
                    .projections
                    .get(&request_id)
                    .map_or(0, |projection| projection.candidate_index);
                if let Some(completion) =
                    continue_projection(&mut state, request_id, candidate_index, None)
                {
                    completions.push(completion);
                }
            }
            Resolution::Missing => {
                let candidate_index = state
                    .projections
                    .get(&request_id)
                    .map_or(0, |projection| projection.candidate_index);
                if let Some(completion) =
                    continue_projection(&mut state, request_id, candidate_index + 1, None)
                {
                    completions.push(completion);
                }
            }
            Resolution::Failed(error) => {
                let candidate_index = state
                    .projections
                    .get(&request_id)
                    .map_or(0, |projection| projection.candidate_index);
                if let Some(completion) = continue_projection(
                    &mut state,
                    request_id,
                    candidate_index + 1,
                    Some(Arc::clone(error)),
                ) {
                    completions.push(completion);
                }
            }
        }
    }
    let background_result = match &resolution {
        Resolution::Ready { .. } => BackgroundResult::Ready,
        Resolution::Cached => BackgroundResult::Cached,
        Resolution::Missing => BackgroundResult::Missing,
        Resolution::Failed(_) => BackgroundResult::Failed,
    };
    let mut background_completions = Vec::new();
    for subscriber in record.preparations {
        background_completions.push(subscriber.completion);
    }
    if !background_completions.is_empty()
        && let Resolution::Failed(error) = &resolution
    {
        warn!(
            source_id = %work.source.source_id,
            %error,
            "could not prepare one source artwork image"
        );
    }
    drop(state);
    for completion in background_completions {
        let _ = completion.send(background_result);
    }
    drop(resolution);
    send_completions(completions);
    shared.wake.notify_all();
}

fn restart_projection(state: &mut State, request_id: RequestId) -> Option<LeaseCompletion> {
    if let Some(projection) = state.projections.get_mut(&request_id) {
        projection.failures.clear();
    }
    continue_projection(state, request_id, 0, None)
}

fn continue_projection(
    state: &mut State,
    request_id: RequestId,
    candidate_index: usize,
    failure: Option<Arc<str>>,
) -> Option<LeaseCompletion> {
    if let Some(failure) = failure
        && let Some(projection) = state.projections.get_mut(&request_id)
    {
        projection.failures.push(failure);
    }
    let next = state.projections.get(&request_id).and_then(|projection| {
        projection
            .request
            .binding
            .candidates()
            .get(candidate_index)
            .cloned()
            .map(|candidate| {
                (
                    projection.source.clone(),
                    candidate_request(&projection.request, candidate),
                    projection.priority,
                )
            })
    });
    if let Some((source, request, priority)) = next {
        let job = enqueue_projection(state, source, request, request_id, priority);
        if let Some(projection) = state.projections.get_mut(&request_id) {
            projection.candidate_index = candidate_index;
            projection.job = job;
        }
        return None;
    }

    let projection = state.projections.remove(&request_id)?;
    let outcome = if projection.failures.is_empty() {
        ArtworkOutcome::Missing
    } else {
        let message = projection
            .failures
            .iter()
            .map(AsRef::as_ref)
            .collect::<Vec<_>>()
            .join("; ");
        ArtworkOutcome::Failed(message.into())
    };
    Some((projection.completion, outcome))
}

fn lock_state(shared: &Shared) -> MutexGuard<'_, State> {
    shared
        .state
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

fn lock_cache_commit(shared: &Shared) -> MutexGuard<'_, ()> {
    shared
        .cache_commit
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

#[cfg(test)]
mod tests {
    use super::*;
    use sources::NativeImageRef;

    #[test]
    fn durable_no_art_binding_completes_as_missing() {
        let directory = tempfile::tempdir().expect("cache");
        let runtime = tokio::runtime::Builder::new_current_thread()
            .build()
            .expect("runtime");
        let pipeline = Pipeline::new(directory.path(), runtime.handle().clone()).expect("pipeline");
        let summary = pipeline
            .prefetch_source_artwork(
                SourceImages::cache_only(SourceId::new("source")),
                Arc::from([br#"{"no_art":true}"#.to_vec()]),
                &|_, _| {},
                &|| false,
            )
            .expect("prepare no-art");
        assert_eq!(summary.missing, 1);
        assert_eq!(summary.failed, 0);
    }

    #[test]
    fn identical_foreground_requests_share_one_fetch_job() {
        let mut state = State::default();
        let source = SourceImages::cache_only(SourceId::new("source"));
        let request = CandidateRequest {
            candidate: Candidate::Native(NativeImageRef::new("album", Some("tag".to_string()))),
            fetch_size: 256,
            render_size: 144,
            external: ExternalPolicy::default(),
        };
        let first = enqueue_projection(
            &mut state,
            source.clone(),
            request.clone(),
            RequestId(1),
            JobPriority::Foreground,
        );
        let second = enqueue_projection(
            &mut state,
            source,
            request,
            RequestId(2),
            JobPriority::Foreground,
        );

        assert_eq!(first, second);
        assert_eq!(state.jobs.len(), 1);
        assert_eq!(state.foreground.len(), 1);
        let job = state.jobs.get(&first).expect("shared job");
        assert_eq!(job.subscribers.len(), 2);
        assert_eq!(job.foreground_subscribers.len(), 2);
    }
}

fn send_completions(completions: Vec<LeaseCompletion>) {
    for (completion, outcome) in completions {
        let _ = completion.send(outcome);
    }
}
