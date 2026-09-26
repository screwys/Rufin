use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet, VecDeque};
use std::path::Path;
use std::sync::{Arc, Condvar, Mutex, MutexGuard, Weak};
use std::thread;

use futures_util::{StreamExt, stream::FuturesUnordered};
use sources::{ImageSize, SourceId};
use tokio::runtime::Handle;
use tokio::sync::oneshot;
use tokio_util::sync::CancellationToken;
use tracing::warn;

use crate::cache::FilesystemCache;
use crate::decode::{decode_cached, decode_normalized, decode_original, normalize_for_cache};
use crate::fetch::{FetchContext, FetchOutcome};
use crate::selection::Candidate;
use crate::{
    ArtworkBinding, ArtworkError, ArtworkKey, ArtworkLoad, ArtworkPreparation, ArtworkRequest,
    DecodedImage, ExternalPolicy, LoadedArtwork, PendingArtwork, RequestId, SourceResolver,
};

pub(crate) const WORKERS: usize = 4;
pub(crate) const PREPARATION_WORKERS: usize = WORKERS - 1;
// Keep every worker fed without mirroring the selected source in the job table.
pub(crate) const PREPARATION_WINDOW: usize = PREPARATION_WORKERS * 4;
const MAX_DECODED_INDEX_ENTRIES: usize = 4_096;
const SOURCE_ARTWORK_SIZE: u32 = 256;
pub(crate) const CACHED_IMAGE_SIZES: &[ImageSize] = &[
    ImageSize::Original,
    ImageSize::Thumbnail(512),
    ImageSize::Thumbnail(256),
    ImageSize::Thumbnail(96),
];

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
    external_epoch: u64,
    source_epochs: HashMap<SourceId, u64>,
    foreground: VecDeque<ArtworkKey>,
    preparations: VecDeque<ArtworkKey>,
    jobs: HashMap<ArtworkKey, JobRecord>,
    decoded_index: DecodedIndex,
}

struct JobRecord {
    request: Arc<ArtworkRequest>,
    subscribers: HashMap<RequestId, Subscriber>,
    active: bool,
}

struct Subscriber {
    priority: JobPriority,
    completion: oneshot::Sender<Resolution>,
}

struct Work {
    key: ArtworkKey,
    request: Arc<ArtworkRequest>,
    source_epoch: u64,
    external_epoch: u64,
    decode: bool,
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
enum JobPriority {
    Preparation,
    Foreground,
}

#[derive(Default)]
struct DecodedIndex {
    entries: HashMap<ArtworkKey, DecodedEntry>,
    sizes: HashMap<(String, String), BTreeMap<u32, HashSet<ArtworkKey>>>,
    eviction_order: BTreeSet<DecodedAccess>,
    next_access: u64,
}

struct DecodedEntry {
    source_id: Option<SourceId>,
    image: Weak<DecodedImage>,
    last_used: u64,
}

#[derive(Clone, Eq, Ord, PartialEq, PartialOrd)]
struct DecodedAccess {
    last_used: u64,
    key: ArtworkKey,
}

#[derive(Clone)]
pub(crate) enum Resolution {
    Ready {
        image: Arc<DecodedImage>,
        original: Option<Arc<[u8]>>,
        cached: bool,
    },
    Cached,
    Fetched,
    Missing,
    Failed(Arc<str>),
    Invalidated,
}

impl Pipeline {
    pub(crate) fn install_database(&self, database: Arc<library::Database>) {
        self.shared.fetch.install_database(database);
    }
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
            let binding = ArtworkBinding::opaque(binding);
            for candidate in &binding.candidates {
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
    pub(crate) fn new(
        cache_root: &Path,
        runtime: Handle,
        source_resolver: Arc<Mutex<Option<Arc<SourceResolver>>>>,
    ) -> Result<Self, ArtworkError> {
        let cache = FilesystemCache::new(cache_root.to_path_buf())?;
        let fetch = FetchContext::new(source_resolver);
        let shared = Arc::new(Shared {
            runtime,
            cache,
            fetch,
            cache_commit: Mutex::new(()),
            state: Mutex::new(State {
                next_request: 1,
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

    pub(crate) fn load(self: &Arc<Self>, request: ArtworkRequest) -> ArtworkLoad {
        self.submit(request, JobPriority::Foreground)
    }

    fn submit(self: &Arc<Self>, request: ArtworkRequest, priority: JobPriority) -> ArtworkLoad {
        let mut state = lock_state(&self.shared);
        let key = request_key(&state, &request);
        if let Some(image) = decoded_from_memory(&mut state, &request, &key) {
            return ArtworkLoad::Ready(LoadedArtwork {
                image,
                original: None,
            });
        }
        if request.binding.candidates.is_empty() {
            return ArtworkLoad::Missing;
        }
        let request_id = RequestId(state.next_request);
        state.next_request = state.next_request.wrapping_add(1).max(1);
        let (completion, receiver) = oneshot::channel();
        let job = enqueue(
            &mut state,
            request,
            request_id,
            Subscriber {
                priority,
                completion,
            },
        );
        drop(state);
        self.shared.wake.notify_all();
        ArtworkLoad::Pending(PendingArtwork {
            job,
            request_id,
            completion: Some(receiver),
            pipeline: Arc::clone(self),
        })
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

    pub(crate) async fn prefetch_source_artwork(
        self: &Arc<Self>,
        artwork: &[Vec<u8>],
        cancelled: &CancellationToken,
    ) -> Result<ArtworkPreparation, ArtworkError> {
        let prepare = async {
            let mut bindings = artwork.iter();
            let mut pending = FuturesUnordered::new();
            let mut summary = ArtworkPreparation {
                total: artwork.len(),
                ..ArtworkPreparation::default()
            };
            loop {
                while pending.len() < PREPARATION_WINDOW {
                    let Some(binding) = bindings.next() else {
                        break;
                    };
                    let request = ArtworkRequest::new(
                        ArtworkBinding::opaque(binding),
                        SOURCE_ARTWORK_SIZE,
                        SOURCE_ARTWORK_SIZE,
                    );
                    let load = self.submit(request, JobPriority::Preparation);
                    pending.push(async move {
                        match load {
                            ArtworkLoad::Ready(loaded) => Resolution::Ready {
                                image: loaded.image,
                                original: loaded.original,
                                cached: true,
                            },
                            ArtworkLoad::Missing => Resolution::Missing,
                            ArtworkLoad::Pending(pending) => pending.finish_resolution().await,
                        }
                    });
                }
                let Some(result) = pending.next().await else {
                    break;
                };
                match result {
                    Resolution::Ready { cached: true, .. } | Resolution::Cached => {
                        summary.ready += 1;
                        summary.cached += 1;
                    }
                    Resolution::Ready { cached: false, .. } | Resolution::Fetched => {
                        summary.ready += 1
                    }
                    Resolution::Missing => summary.missing += 1,
                    Resolution::Failed(error) => {
                        warn!(%error, "could not prepare one source artwork image");
                        summary.failed += 1;
                    }
                    Resolution::Invalidated => return Err(ArtworkError::Cancelled),
                }
            }
            Ok(summary)
        };
        cancelled
            .run_until_cancelled(prepare)
            .await
            .ok_or(ArtworkError::Cancelled)?
    }

    pub(crate) fn cancel(&self, key: &ArtworkKey, request_id: RequestId) {
        let mut state = lock_state(&self.shared);
        if state
            .jobs
            .get_mut(key)
            .and_then(|record| record.subscribers.remove(&request_id))
            .is_none()
        {
            return;
        }
        reschedule_or_remove(&mut state, key, false);
        drop(state);
        self.shared.wake.notify_all();
    }

    pub(crate) fn cache_only_file(
        &self,
        request: &ArtworkRequest,
        sizes: &[ImageSize],
    ) -> Option<std::path::PathBuf> {
        request.binding.candidates.iter().find_map(|candidate| {
            cached_leaf_file(&self.shared, candidate, sizes, &request.external)
        })
    }

    pub(crate) fn key(&self, request: &ArtworkRequest) -> ArtworkKey {
        request_key(&lock_state(&self.shared), request)
    }

    pub(crate) fn retry_external(&self) -> Result<(), ArtworkError> {
        let commit = lock_cache_commit(&self.shared);
        self.shared.cache.retry_external()?;
        let mut state = lock_state(&self.shared);
        state.external_epoch = state.external_epoch.wrapping_add(1);
        drop(state);
        drop(commit);
        self.shared.wake.notify_all();
        Ok(())
    }

    pub(crate) fn invalidate_source(&self, source_id: &SourceId) -> Result<(), ArtworkError> {
        self.invalidate_source_images(source_id, None)
    }

    pub(crate) fn invalidate_image(&self, binding: &ArtworkBinding) -> Result<bool, ArtworkError> {
        let Some(candidate) = binding.candidates.first() else {
            return Ok(false);
        };
        let Some(source_id) = candidate.source_id() else {
            return Ok(false);
        };
        self.invalidate_source_images(source_id, Some(candidate))?;
        Ok(true)
    }

    fn invalidate_source_images(
        &self,
        source_id: &SourceId,
        image: Option<&Candidate>,
    ) -> Result<(), ArtworkError> {
        let commit = lock_cache_commit(&self.shared);
        match image {
            Some(image) => self.shared.cache.invalidate_image(image)?,
            None => self.shared.cache.invalidate_source(source_id)?,
        }
        let mut state = lock_state(&self.shared);
        *state.source_epochs.entry(source_id.clone()).or_default() = state
            .source_epochs
            .get(source_id)
            .copied()
            .unwrap_or_default()
            .wrapping_add(1);
        state.decoded_index.invalidate_source(source_id);
        let keys = state
            .jobs
            .iter()
            .filter(|(_, record)| record.request.binding.source_id() == Some(source_id))
            .map(|(key, _)| key.clone())
            .collect::<Vec<_>>();
        let completions = keys
            .iter()
            .filter_map(|key| remove_job(&mut state, key))
            .flat_map(|record| {
                record
                    .subscribers
                    .into_values()
                    .map(|subscriber| subscriber.completion)
            })
            .collect::<Vec<_>>();
        drop(state);
        drop(commit);
        for completion in completions {
            let _ = completion.send(Resolution::Invalidated);
        }
        Ok(())
    }
}

fn request_key(state: &State, request: &ArtworkRequest) -> ArtworkKey {
    job_key(
        request,
        source_epoch(state, &request.binding),
        state.external_epoch,
    )
}

fn decoded_from_memory(
    state: &mut State,
    request: &ArtworkRequest,
    key: &ArtworkKey,
) -> Option<Arc<DecodedImage>> {
    if request.fetch_size == ImageSize::Original {
        return None;
    }
    if request.binding.has_external() && !request.external.allow_cached {
        return None;
    }
    state.decoded_index.get_for_request(key)
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

    fn get_for_request(&mut self, exact_key: &ArtworkKey) -> Option<Arc<DecodedImage>> {
        if let Some(image) = self.get(exact_key) {
            return Some(image);
        }
        let reusable = self
            .sizes
            .get(&exact_key.reuse_group())
            .into_iter()
            .flat_map(|sizes| sizes.range(exact_key.render_size..))
            .flat_map(|(_, keys)| keys.iter().cloned())
            .collect::<Vec<_>>();
        reusable
            .into_iter()
            .find_map(|reusable| self.get(&reusable))
    }

    fn insert(&mut self, key: ArtworkKey, source_id: Option<SourceId>, image: Arc<DecodedImage>) {
        self.insert_with_limit(key, source_id, image, MAX_DECODED_INDEX_ENTRIES);
    }

    fn insert_with_limit(
        &mut self,
        key: ArtworkKey,
        source_id: Option<SourceId>,
        image: Arc<DecodedImage>,
        max_entries: usize,
    ) {
        self.remove_entry(&key);
        let last_used = self.next_access();
        self.sizes
            .entry(key.reuse_group())
            .or_default()
            .entry(key.render_size)
            .or_default()
            .insert(key.clone());
        self.entries.insert(
            key.clone(),
            DecodedEntry {
                source_id,
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
            .filter(|(_, entry)| entry.source_id.as_ref() == Some(source_id))
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
        let remove_family = self.sizes.get_mut(&key.reuse_group()).is_some_and(|sizes| {
            if let Some(keys) = sizes.get_mut(&key.render_size) {
                keys.remove(key);
                if keys.is_empty() {
                    sizes.remove(&key.render_size);
                }
            }
            sizes.is_empty()
        });
        if remove_family {
            self.sizes.remove(&key.reuse_group());
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

impl JobRecord {
    fn has_interest(&self) -> bool {
        !self.subscribers.is_empty()
    }

    fn priority(&self) -> JobPriority {
        self.subscribers
            .values()
            .map(|subscriber| subscriber.priority)
            .max()
            .unwrap_or(JobPriority::Preparation)
    }
}

fn enqueue(
    state: &mut State,
    request: ArtworkRequest,
    request_id: RequestId,
    subscriber: Subscriber,
) -> ArtworkKey {
    // The job identity stays stable across retries; each worker snapshots the current epochs.
    let key = job_key(&request, 0, 0);
    let foreground = subscriber.priority == JobPriority::Foreground;
    let record = state.jobs.entry(key.clone()).or_insert_with(|| JobRecord {
        request: Arc::new(request),
        subscribers: HashMap::new(),
        active: false,
    });
    let existing = record.has_interest();
    record.subscribers.insert(request_id, subscriber);
    if !record.active {
        queue(state, key.clone(), existing && foreground);
    }
    key
}

fn reschedule_or_remove(state: &mut State, key: &ArtworkKey, front: bool) {
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

fn remove_job(state: &mut State, key: &ArtworkKey) -> Option<JobRecord> {
    remove_queued(state, key);
    state.jobs.remove(key)
}

fn queue(state: &mut State, key: ArtworkKey, front: bool) {
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

fn remove_queued(state: &mut State, key: &ArtworkKey) {
    state.foreground.retain(|queued| queued != key);
    state.preparations.retain(|queued| queued != key);
}

fn job_key(request: &ArtworkRequest, source_epoch: u64, external_epoch: u64) -> ArtworkKey {
    ArtworkKey::derive(
        &request.binding,
        (request.fetch_size, request.render_size),
        request.binding.has_external().then_some(&request.external),
        !request.cache_only,
        (source_epoch, external_epoch),
    )
}

fn source_epoch(state: &State, binding: &ArtworkBinding) -> u64 {
    binding
        .source_id()
        .and_then(|source_id| state.source_epochs.get(source_id))
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
            let request = Arc::clone(&record.request);
            let decode = record.priority() == JobPriority::Foreground;
            return Work {
                key,
                source_epoch: source_epoch(&state, &request.binding),
                external_epoch: state.external_epoch,
                request,
                decode,
            };
        }
        state = shared
            .wake
            .wait(state)
            .unwrap_or_else(std::sync::PoisonError::into_inner);
    }
}

fn resolve(shared: &Shared, work: &Work) -> Resolution {
    let mut failure = None;
    for candidate in &work.request.binding.candidates {
        match resolve_candidate(shared, work, candidate) {
            Resolution::Missing => {}
            Resolution::Failed(error) => failure = Some(error),
            resolved => return resolved,
        }
    }
    failure
        .map(Resolution::Failed)
        .unwrap_or(Resolution::Missing)
}

fn cached_leaf_file(
    shared: &Shared,
    candidate: &Candidate,
    sizes: &[ImageSize],
    external: &ExternalPolicy,
) -> Option<std::path::PathBuf> {
    if candidate.is_external() && !external.allow_cached {
        return None;
    }
    sizes.iter().find_map(|size| {
        shared
            .cache
            .ready_entry(candidate, *size)
            .map(|entry| entry.path)
    })
}

fn resolve_candidate(shared: &Shared, work: &Work, candidate: &Candidate) -> Resolution {
    let result = resolve_request(shared, work, candidate)
        .unwrap_or_else(|error| Resolution::Failed(error.into()));
    let request = &work.request;
    if request.fetch_size == ImageSize::Original
        && matches!(result, Resolution::Missing | Resolution::Failed(_))
        && let Some(path) = cached_leaf_file(
            shared,
            candidate,
            &CACHED_IMAGE_SIZES[1..],
            &request.external,
        )
        && let Ok(bytes) = std::fs::read(path)
        && let Ok(image) = decode_original(
            &bytes,
            job_key(request, work.source_epoch, work.external_epoch),
            request.render_size,
        )
    {
        return Resolution::Ready {
            image: Arc::new(image),
            original: Some(Arc::from(bytes)),
            cached: true,
        };
    }
    result
}

fn resolve_request(
    shared: &Shared,
    work: &Work,
    candidate: &Candidate,
) -> Result<Resolution, String> {
    let request = &work.request;
    let artwork_key = job_key(request, work.source_epoch, work.external_epoch);
    let may_read_cache = !candidate.is_external() || request.external.allow_cached;
    if may_read_cache {
        if let Some(entry) = shared.cache.ready_entry(candidate, request.fetch_size) {
            if !work.decode {
                return Ok(Resolution::Cached);
            }
            let loaded = if request.fetch_size == ImageSize::Original {
                std::fs::read(&entry.path)
                    .map_err(ArtworkError::Cache)
                    .and_then(|bytes| {
                        decode_original(&bytes, artwork_key.clone(), request.render_size)
                            .map(|image| (image, Some(Arc::from(bytes))))
                    })
            } else {
                decode_cached(&entry.path, artwork_key.clone(), request.render_size)
                    .map(|image| (image, None))
            };
            match loaded {
                Ok((image, original)) => {
                    return Ok(Resolution::Ready {
                        image: Arc::new(image),
                        original,
                        cached: true,
                    });
                }
                Err(_) => shared.cache.remove_ready(&entry.path),
            }
        }
        if let ImageSize::Thumbnail(_) = request.fetch_size
            && let Some(entry) = shared.cache.ready_entry(candidate, ImageSize::Original)
        {
            if let Ok(bytes) = std::fs::read(&entry.path) {
                match store_image(shared, work, candidate, bytes, true) {
                    Ok(resolved) => return Ok(resolved),
                    Err(ArtworkError::Decode(_)) => {}
                    Err(error) => return Err(error.to_string()),
                }
            }
            shared.cache.remove_ready(&entry.path);
        }
        if shared.cache.is_missing(candidate, request.fetch_size) {
            return Ok(Resolution::Missing);
        }
    }
    if request.cache_only || (candidate.is_external() && !request.external.allow_network) {
        return Ok(Resolution::Missing);
    }
    match shared.fetch.fetch(
        &shared.runtime,
        candidate,
        request.fetch_size,
        &request.external,
    )? {
        FetchOutcome::Ready(bytes) => {
            store_image(shared, work, candidate, bytes, false).map_err(|error| error.to_string())
        }
        FetchOutcome::Missing => {
            mark_missing(shared, work, candidate).map_err(|error| error.to_string())?;
            Ok(Resolution::Missing)
        }
    }
}

fn store_image(
    shared: &Shared,
    work: &Work,
    candidate: &Candidate,
    bytes: Vec<u8>,
    cached: bool,
) -> Result<Resolution, ArtworkError> {
    let request = &work.request;
    let thumbnail_size = match request.fetch_size {
        ImageSize::Original => request.render_size.max(SOURCE_ARTWORK_SIZE),
        ImageSize::Thumbnail(size) => size,
    };
    let thumbnail = normalize_for_cache(&bytes, thumbnail_size)?;
    {
        let _commit = lock_cache_commit(shared);
        if !work_is_current(&lock_state(shared), work) {
            return Ok(Resolution::Invalidated);
        }
        shared.cache.write_ready(
            candidate,
            ImageSize::Thumbnail(thumbnail_size),
            thumbnail.bytes(),
        )?;
        if request.fetch_size == ImageSize::Original {
            shared
                .cache
                .write_ready(candidate, ImageSize::Original, &bytes)?;
        }
    }
    if !work.decode {
        return Ok(if cached {
            Resolution::Cached
        } else {
            Resolution::Fetched
        });
    }
    let image = decode_normalized(
        thumbnail,
        job_key(request, work.source_epoch, work.external_epoch),
        request.render_size,
    )?;
    Ok(Resolution::Ready {
        image: Arc::new(image),
        original: (request.fetch_size == ImageSize::Original).then(|| Arc::from(bytes)),
        cached,
    })
}

fn mark_missing(shared: &Shared, work: &Work, candidate: &Candidate) -> std::io::Result<bool> {
    let _commit = lock_cache_commit(shared);
    let state = lock_state(shared);
    if !work_is_current(&state, work) {
        return Ok(false);
    }
    drop(state);
    shared
        .cache
        .mark_missing(candidate, work.request.fetch_size)?;
    Ok(true)
}

fn work_is_current(state: &State, work: &Work) -> bool {
    source_epoch(state, &work.request.binding) == work.source_epoch
        && (!work.request.binding.has_external() || state.external_epoch == work.external_epoch)
}

fn finish(shared: &Shared, work: Work, resolution: Resolution) {
    let mut state = lock_state(shared);
    // An invalidated job can have a replacement under the same key.
    if !state
        .jobs
        .get(&work.key)
        .is_some_and(|record| Arc::ptr_eq(&record.request, &work.request))
    {
        return;
    }
    let mut record = remove_job(&mut state, &work.key).expect("active artwork job");
    let retry = !work_is_current(&state, &work);
    if !retry
        && record.has_interest()
        && let Resolution::Ready { image, .. } = &resolution
    {
        state.decoded_index.insert(
            image.key().clone(),
            work.request.binding.source_id().cloned(),
            Arc::clone(image),
        );
    }
    let completions = record
        .subscribers
        .extract_if(|_, subscriber| {
            !retry
                && !(subscriber.priority == JobPriority::Foreground
                    && matches!(resolution, Resolution::Cached | Resolution::Fetched))
        })
        .map(|(_, subscriber)| subscriber.completion)
        .collect::<Vec<_>>();
    if record.has_interest() {
        // Foreground callers joining a preparation may still need a disk decode.
        record.active = false;
        state.jobs.insert(work.key.clone(), record);
        queue(&mut state, work.key, true);
    }
    drop(state);
    for completion in completions {
        let _ = completion.send(resolution.clone());
    }
    shared.wake.notify_all();
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

    #[tokio::test]
    async fn shared_foreground_and_preparation_disk_hit_counts_as_cached() {
        let directory = tempfile::tempdir().unwrap();
        let cache = FilesystemCache::new(directory.path().to_path_buf()).unwrap();
        let request = ArtworkRequest {
            binding: ArtworkBinding::opaque(
                &sources::native_artwork_binding("source", &NativeImageRef::new("album", None))
                    .unwrap(),
            ),
            fetch_size: ImageSize::Thumbnail(256),
            render_size: 144,
            external: ExternalPolicy::default(),
            cache_only: false,
        };
        let mut png = std::io::Cursor::new(Vec::new());
        image::DynamicImage::ImageRgba8(image::RgbaImage::from_pixel(
            16,
            16,
            image::Rgba([40, 80, 120, 255]),
        ))
        .write_to(&mut png, image::ImageFormat::Png)
        .unwrap();
        cache
            .write_ready(
                request.binding.candidates.first().unwrap(),
                ImageSize::Thumbnail(256),
                png.get_ref(),
            )
            .unwrap();
        // Drive one shared job explicitly so worker scheduling cannot separate its subscribers.
        let mut state = State::default();
        let request_id = RequestId(1);
        let (completion, foreground) = oneshot::channel();
        enqueue(
            &mut state,
            request.clone(),
            request_id,
            Subscriber {
                priority: JobPriority::Foreground,
                completion,
            },
        );
        let (completion, background) = oneshot::channel();
        enqueue(
            &mut state,
            request,
            RequestId(2),
            Subscriber {
                priority: JobPriority::Preparation,
                completion,
            },
        );
        assert_eq!(state.jobs.len(), 1);
        let shared = Shared {
            runtime: Handle::current(),
            cache,
            fetch: FetchContext::new(Arc::new(Mutex::new(None))),
            cache_commit: Mutex::new(()),
            state: Mutex::new(state),
            wake: Condvar::new(),
        };
        let work = next_work(&shared, false);
        let resolution = resolve(&shared, &work);
        finish(&shared, work, resolution);
        assert!(matches!(
            foreground.await.unwrap(),
            Resolution::Ready { .. }
        ));
        assert!(matches!(
            background.await.unwrap(),
            Resolution::Ready { cached: true, .. }
        ));
    }

    #[tokio::test]
    async fn durable_no_art_binding_completes_as_missing() {
        let directory = tempfile::tempdir().expect("cache");
        let pipeline = Arc::new(
            Pipeline::new(
                directory.path(),
                Handle::current(),
                Arc::new(Mutex::new(None)),
            )
            .expect("pipeline"),
        );
        let summary = pipeline
            .prefetch_source_artwork(&[br#"{"no_art":true}"#.to_vec()], &CancellationToken::new())
            .await
            .expect("prepare no-art");
        assert_eq!(summary.missing, 1);
        assert_eq!(summary.failed, 0);
    }

    #[test]
    fn identical_foreground_requests_share_one_fetch_job() {
        let mut state = State::default();
        let request = ArtworkRequest {
            binding: ArtworkBinding::opaque(
                &sources::native_artwork_binding(
                    "source",
                    &NativeImageRef::new("album", Some("tag".to_string())),
                )
                .unwrap(),
            ),
            fetch_size: ImageSize::Thumbnail(256),
            render_size: 144,
            external: ExternalPolicy::default(),
            cache_only: false,
        };
        let first = enqueue(
            &mut state,
            request.clone(),
            RequestId(1),
            Subscriber {
                priority: JobPriority::Foreground,
                completion: oneshot::channel().0,
            },
        );
        let second = enqueue(
            &mut state,
            request,
            RequestId(2),
            Subscriber {
                priority: JobPriority::Foreground,
                completion: oneshot::channel().0,
            },
        );

        assert_eq!(first, second);
        assert_eq!(state.jobs.len(), 1);
        assert_eq!(state.foreground.len(), 1);
        let job = state.jobs.get(&first).expect("shared job");
        assert_eq!(job.subscribers.len(), 2);
        assert_eq!(job.priority(), JobPriority::Foreground);
    }
}
