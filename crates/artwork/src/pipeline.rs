use std::collections::{HashMap, VecDeque};
use std::path::Path;
use std::sync::{Arc, Condvar, Mutex, MutexGuard, Weak};
use std::thread;

use sources::{ImageSize, SourceId};
use tokio::runtime::Handle;
use tokio::sync::oneshot;

use crate::cache::FilesystemCache;
use crate::decode::{cached_poster, decode_display, decode_original};
use crate::fetch::{FetchContext, FetchOutcome};
use crate::selection::Candidate;
use crate::{
    ArtworkBinding, ArtworkError, ArtworkKey, ArtworkLoad, ArtworkRequest, CacheResolver,
    DecodedImage, PendingArtwork, RequestId, RgbaImage, SourceResolver,
};

pub(crate) const WORKERS: usize = 4;
pub(crate) struct Pipeline {
    shared: Arc<Shared>,
}

struct Shared {
    runtime: Handle,
    cache: FilesystemCache,
    fetch: FetchContext,
    cache_commit: Mutex<()>,
    cache_resolver: Mutex<Option<Arc<CacheResolver>>>,
    previews: Mutex<VecDeque<(String, String, Arc<crate::ArtworkPreview>)>>,
    state: Mutex<State>,
    wake: Condvar,
}

#[derive(Default)]
struct State {
    next_request: u64,
    cache_epoch: u64,
    external_epoch: u64,
    source_epochs: HashMap<SourceId, u64>,
    foreground: VecDeque<ArtworkKey>,
    background: VecDeque<ArtworkKey>,
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
    decode: bool,
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
    Background,
    Foreground,
}

#[derive(Default)]
struct DecodedIndex {
    entries: HashMap<ArtworkKey, DecodedEntry>,
}

struct DecodedEntry {
    source_id: Option<SourceId>,
    image: Weak<DecodedImage>,
}

#[derive(Clone)]
pub(crate) enum Resolution {
    Ready {
        path: std::path::PathBuf,
        image: Option<Arc<DecodedImage>>,
    },
    Missing,
    Failed(Arc<str>),
    Invalidated,
}

impl Pipeline {
    pub(crate) fn preview(
        &self,
        binding: &ArtworkBinding,
        allow_external: bool,
    ) -> Option<Arc<crate::ArtworkPreview>> {
        prepared_preview(&self.shared, binding, allow_external)
    }

    pub(crate) fn cache(&self) -> &FilesystemCache {
        &self.shared.cache
    }

    pub(crate) fn import_missing_cache_file(
        &self,
        key: &str,
        path: &Path,
    ) -> Result<bool, ArtworkError> {
        let _commit = lock_cache_commit(&self.shared);
        Ok(self.shared.cache.import_missing(key, path)?)
    }

    pub(crate) fn install_cache_resolver(&self, resolver: Arc<CacheResolver>) {
        *self
            .shared
            .cache_resolver
            .lock()
            .unwrap_or_else(|p| p.into_inner()) = Some(resolver);
    }

    pub(crate) fn invalidate_decoded_cache(&self) {
        let commit = lock_cache_commit(&self.shared);
        let mut state = lock_state(&self.shared);
        state.cache_epoch = state.cache_epoch.wrapping_add(1);
        state.decoded_index.entries.clear();
        state.foreground.clear();
        state.background.clear();
        let jobs = std::mem::take(&mut state.jobs);
        drop(state);
        drop(commit);
        for completion in jobs
            .into_values()
            .flat_map(|job| job.subscribers.into_values())
        {
            let _ = completion.completion.send(Resolution::Invalidated);
        }
        self.shared.wake.notify_all();
    }

    pub(crate) fn remove_cache_entries(
        &self,
        entries: &[crate::ArtworkCacheEntry],
    ) -> Result<(), ArtworkError> {
        let _commit = lock_cache_commit(&self.shared);
        self.shared.cache.remove_entries(entries)?;
        Ok(())
    }

    pub(crate) fn finish_cache_clear(&self) -> Result<(), ArtworkError> {
        let _commit = lock_cache_commit(&self.shared);
        self.shared.cache.clear_missing()?;
        lock_state(&self.shared).decoded_index.entries.clear();
        Ok(())
    }

    pub(crate) fn cache_revision(&self) -> u64 {
        lock_state(&self.shared).cache_epoch
    }

    pub(crate) fn install_database(&self, database: Arc<library::Database>) {
        self.shared.fetch.install_database(database);
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
            cache_resolver: Mutex::new(None),
            previews: Mutex::new(VecDeque::new()),
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
        let mut state = lock_state(&self.shared);
        let key = request_key(&state, &request);
        if let Some(image) = decoded_from_memory(&mut state, &request, &key) {
            return ArtworkLoad::Ready(image);
        }
        self.submit(request, JobPriority::Foreground, true, state)
            .map_or(ArtworkLoad::Missing, ArtworkLoad::Pending)
    }

    fn submit(
        self: &Arc<Self>,
        request: ArtworkRequest,
        priority: JobPriority,
        decode: bool,
        mut state: MutexGuard<'_, State>,
    ) -> Option<PendingArtwork> {
        if request.binding.candidates.is_empty() {
            return None;
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
                decode,
                completion,
            },
        );
        drop(state);
        self.shared.wake.notify_all();
        Some(PendingArtwork {
            job,
            request_id,
            completion: Some(receiver),
            pipeline: Arc::clone(self),
        })
    }

    pub(crate) async fn cache_image(
        self: &Arc<Self>,
        request: ArtworkRequest,
        background: bool,
    ) -> Resolution {
        let priority = if background {
            JobPriority::Background
        } else {
            JobPriority::Foreground
        };
        let submitted = self.submit(request, priority, false, lock_state(&self.shared));
        match submitted {
            Some(pending) => pending.finish_resolution().await,
            None => Resolution::Missing,
        }
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

    pub(crate) fn cache_only_file(&self, request: &ArtworkRequest) -> Option<std::path::PathBuf> {
        request.binding.candidates.iter().find_map(|candidate| {
            if candidate.is_external() && !request.external.allow_cached {
                return None;
            }
            let display = match request.fetch_size {
                ImageSize::Thumbnail(size) => self.shared.cache.display_path(candidate, size),
                ImageSize::Original => None,
            };
            display
                .or_else(|| cached_leaf_file(&self.shared, candidate))
                .or_else(|| {
                    matches!(request.fetch_size, ImageSize::Thumbnail(_))
                        .then(|| self.shared.cache.best_display(candidate))
                        .flatten()
                })
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
        {
            let mut previews = self
                .shared
                .previews
                .lock()
                .unwrap_or_else(|p| p.into_inner());
            match image {
                Some(image) => previews.retain(|(key, _, _)| key != &image.key),
                None => previews.clear(),
            }
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
    if request.binding.has_external() && !request.external.allow_cached {
        return None;
    }
    state.decoded_index.get_for_request(key)
}

impl DecodedIndex {
    fn get_for_request(&mut self, exact_key: &ArtworkKey) -> Option<Arc<DecodedImage>> {
        if let Some(image) = self
            .entries
            .get(exact_key)
            .and_then(|entry| entry.image.upgrade())
        {
            return Some(image);
        }
        self.entries
            .retain(|_, entry| entry.image.strong_count() > 0);
        self.entries
            .iter()
            .filter(|(key, _)| {
                key.same_image(exact_key) && key.render_size >= exact_key.render_size
            })
            .filter_map(|(key, entry)| entry.image.upgrade().map(|image| (key.render_size, image)))
            .min_by_key(|(size, _)| *size)
            .map(|(_, image)| image)
    }

    fn insert(&mut self, key: ArtworkKey, source_id: Option<SourceId>, image: Arc<DecodedImage>) {
        self.entries
            .retain(|_, entry| entry.image.strong_count() > 0);
        self.entries.insert(
            key,
            DecodedEntry {
                source_id,
                image: Arc::downgrade(&image),
            },
        );
    }

    fn invalidate_source(&mut self, source_id: &SourceId) {
        self.entries
            .retain(|_, entry| entry.source_id.as_ref() != Some(source_id));
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
            .unwrap_or(JobPriority::Background)
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
        JobPriority::Background => &mut state.background,
    };
    if front {
        queue.push_front(key);
    } else {
        queue.push_back(key);
    }
}

fn remove_queued(state: &mut State, key: &ArtworkKey) {
    state.foreground.retain(|queued| queued != key);
    state.background.retain(|queued| queued != key);
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
        .wrapping_add(state.cache_epoch)
}

fn run_worker(shared: Arc<Shared>, foreground_reserved: bool) {
    let mut completed_work = false;
    loop {
        let work = next_work(&shared, foreground_reserved, &mut completed_work);
        let resolution = resolve(&shared, &work);
        finish(&shared, work, resolution);
        completed_work = true;
    }
}

fn next_work(shared: &Shared, foreground_reserved: bool, completed_work: &mut bool) -> Work {
    let mut state = lock_state(shared);
    loop {
        let key = state.foreground.pop_front().or_else(|| {
            (!foreground_reserved)
                .then(|| state.background.pop_front())
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
            let decode = record
                .subscribers
                .values()
                .any(|subscriber| subscriber.decode);
            return Work {
                key,
                source_epoch: source_epoch(&state, &request.binding),
                external_epoch: state.external_epoch,
                request,
                decode,
            };
        }
        if *completed_work {
            *completed_work = false;
            drop(state);
            process_memory::request_reclaim();
            state = lock_state(shared);
            continue;
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

fn cached_leaf_file(shared: &Shared, candidate: &Candidate) -> Option<std::path::PathBuf> {
    shared.cache.ready_entry(candidate).map(|entry| entry.path)
}

fn resolve_candidate(shared: &Shared, work: &Work, candidate: &Candidate) -> Resolution {
    let result = resolve_request(shared, work, candidate)
        .unwrap_or_else(|error| Resolution::Failed(error.into()));
    if matches!(result, Resolution::Missing | Resolution::Failed(_))
        && matches!(work.request.fetch_size, ImageSize::Thumbnail(_))
        && (!candidate.is_external() || work.request.external.allow_cached)
        && let Some(path) = shared.cache.best_display(candidate)
        && let Ok(cached) = read_display(shared, work, candidate, &path)
    {
        return cached;
    }
    result
}

fn resolve_peer_cache(
    shared: &Shared,
    work: &Work,
    candidate: &Candidate,
) -> Option<(crate::cache::CacheEntry, bool)> {
    if work.request.cache_only {
        return None;
    }
    let resolver = shared
        .cache_resolver
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .clone()?;
    let request = crate::CacheImageRequest {
        family: candidate.key.clone(),
        size: work.request.fetch_size,
    };
    let (key, file) = shared.runtime.block_on(resolver(request))?;
    let _commit = lock_cache_commit(shared);
    if !work_is_current(&lock_state(shared), work) {
        return None;
    }
    shared.cache.import_missing(&key, &file).ok()?;
    let path = shared.cache.file(&key).ok()??;
    Some((
        crate::cache::CacheEntry { path },
        key.starts_with("originals/"),
    ))
}

fn resolve_request(
    shared: &Shared,
    work: &Work,
    candidate: &Candidate,
) -> Result<Resolution, String> {
    let request = &work.request;
    let may_read_cache = !candidate.is_external() || request.external.allow_cached;
    if may_read_cache {
        if let ImageSize::Thumbnail(size) = request.fetch_size
            && let Some(path) = shared.cache.display_path(candidate, size)
        {
            match read_display(shared, work, candidate, &path) {
                Ok(resolved) => return Ok(resolved),
                Err(_) => shared.cache.remove_ready(&path),
            }
        }
        let missing = shared.cache.is_missing(candidate);
        let entry = shared.cache.ready_entry(candidate).map(|entry| (entry, true)).or_else(|| {
            if missing || matches!(&candidate.image, library::ArtworkImage::Local { path, .. }
                if Path::new(path).is_absolute() || path.starts_with("content:") || path.starts_with("rufin-document:"))
            { None } else { resolve_peer_cache(shared, work, candidate) }
        });
        if let Some((entry, original)) = entry {
            let result = if !original {
                read_display(shared, work, candidate, &entry.path)
            } else if !work.decode && request.fetch_size == ImageSize::Original {
                return Ok(Resolution::Ready {
                    path: entry.path,
                    image: None,
                });
            } else {
                std::fs::read(&entry.path)
                    .map_err(ArtworkError::Cache)
                    .and_then(|bytes| {
                        store_image(shared, work, candidate, bytes, Some(entry.path.clone()))
                    })
            };
            match result {
                Ok(resolved) => return Ok(resolved),
                Err(ArtworkError::Cache(error)) if error.kind() != std::io::ErrorKind::NotFound => {
                    return Err(error.to_string());
                }
                Err(ArtworkError::Cache(_)) => {}
                Err(_) => shared.cache.remove_ready(&entry.path),
            }
        }
        if (request.cache_only || candidate.is_external() && !request.external.allow_network)
            && matches!(request.fetch_size, ImageSize::Thumbnail(_))
            && let Some(path) = shared.cache.best_display(candidate)
        {
            match read_display(shared, work, candidate, &path) {
                Ok(resolved) => return Ok(resolved),
                Err(_) => shared.cache.remove_ready(&path),
            }
        }
        if missing {
            return Ok(Resolution::Missing);
        }
    }
    if request.cache_only || (candidate.is_external() && !request.external.allow_network) {
        return Ok(Resolution::Missing);
    }
    if let library::ArtworkImage::Playlist {
        automatic: Some(_),
        resolved,
        ..
    } = &candidate.image
    {
        let Some(recipes) = resolved else {
            let database = shared
                .fetch
                .database()
                .ok_or("Playlist artwork storage is unavailable")?;
            return match shared
                .runtime
                .block_on(database.resolve_playlist_artwork(candidate.clone()))
                .map_err(|error| error.to_string())?
            {
                Some(resolved) => resolve_request(shared, work, &resolved),
                None => Ok(Resolution::Invalidated),
            };
        };
        return compose_playlist(shared, work, candidate, recipes);
    }
    match shared.fetch.fetch(
        &shared.runtime,
        candidate,
        request.fetch_size,
        &request.external,
    )? {
        FetchOutcome::Ready(bytes) => {
            store_image(shared, work, candidate, bytes, None).map_err(|error| error.to_string())
        }
        FetchOutcome::Missing => {
            mark_missing(shared, work, candidate).map_err(|error| error.to_string())?;
            Ok(Resolution::Missing)
        }
    }
}

fn compose_playlist(
    shared: &Shared,
    work: &Work,
    candidate: &Candidate,
    recipes: &[Vec<Candidate>],
) -> Result<Resolution, String> {
    let mut images = Vec::with_capacity(recipes.len());
    let mut failure = None;
    for recipe in recipes {
        for leaf in recipe {
            let binding = ArtworkBinding::opaque(
                &serde_json::to_vec(&[leaf]).map_err(|error| error.to_string())?,
            );
            let request =
                ArtworkRequest::new(binding, 512).with_external(work.request.external.clone());
            let child = Work {
                key: job_key(&request, 0, 0),
                source_epoch: source_epoch(&lock_state(shared), &request.binding),
                external_epoch: work.external_epoch,
                request: Arc::new(request),
                decode: false,
            };
            match resolve_candidate(shared, &child, leaf) {
                Resolution::Ready { path, .. } => {
                    images.push(std::fs::read(path).map_err(|error| error.to_string())?);
                    break;
                }
                Resolution::Missing => {}
                Resolution::Failed(error) => failure = Some(error),
                Resolution::Invalidated => return Ok(Resolution::Invalidated),
            }
        }
    }
    if images.is_empty() {
        if let Some(error) = failure {
            return Ok(Resolution::Failed(error));
        }
        mark_missing(shared, work, candidate).map_err(|error| error.to_string())?;
        return Ok(Resolution::Missing);
    }
    let bytes = crate::collage_png(&images, 1024).map_err(|error| error.to_string())?;
    store_image(shared, work, candidate, bytes, None).map_err(|error| error.to_string())
}

fn read_display(
    shared: &Shared,
    work: &Work,
    candidate: &Candidate,
    path: &Path,
) -> Result<Resolution, ArtworkError> {
    let image = if work.decode {
        let (image, _) = decode_display(
            std::io::BufReader::new(std::fs::File::open(path)?),
            job_key(&work.request, work.source_epoch, work.external_epoch),
            work.request.render_size,
            None,
            true,
        )?;
        let image = image.expect("display decode requested pixels");
        cache_preview(shared, candidate, false, || image.blur_hash());
        Some(Arc::new(image))
    } else {
        None
    };
    Ok(Resolution::Ready {
        path: path.into(),
        image,
    })
}

fn store_image(
    shared: &Shared,
    work: &Work,
    candidate: &Candidate,
    bytes: Vec<u8>,
    cached_original: Option<std::path::PathBuf>,
) -> Result<Resolution, ArtworkError> {
    let request = &work.request;
    let fetched = cached_original.is_none();
    let key = job_key(request, work.source_epoch, work.external_epoch);
    let (path, image) = match request.fetch_size {
        ImageSize::Thumbnail(size) => {
            let (image, encoded) = decode_display(
                std::io::Cursor::new(&bytes),
                key,
                request.render_size,
                Some(size),
                work.decode,
            )?;
            let _commit = lock_cache_commit(shared);
            if !work_is_current(&lock_state(shared), work) {
                return Ok(Resolution::Invalidated);
            }
            let path = shared.cache.write_ready(
                candidate,
                request.fetch_size,
                encoded.as_deref().unwrap_or(&bytes),
            )?;
            (path, image)
        }
        ImageSize::Original => {
            let path = if let Some(path) = cached_original {
                path
            } else {
                let _commit = lock_cache_commit(shared);
                if !work_is_current(&lock_state(shared), work) {
                    return Ok(Resolution::Invalidated);
                }
                shared
                    .cache
                    .write_ready(candidate, ImageSize::Original, &bytes)?
            };
            if !work.decode {
                return Ok(Resolution::Ready { path, image: None });
            }
            let size = request.render_size.max(512);
            let display = shared.cache.display_path(candidate, size);
            let poster = display
                .as_ref()
                .and_then(|path| cached_poster(path, request.render_size).ok().flatten());
            let (image, encoded) = decode_original(
                Arc::from(bytes),
                key,
                request.render_size,
                poster,
                display.is_none().then_some(size),
            )?;
            if let Some(encoded) = encoded {
                let _commit = lock_cache_commit(shared);
                if !work_is_current(&lock_state(shared), work) {
                    return Ok(Resolution::Invalidated);
                }
                if let Err(error) =
                    shared
                        .cache
                        .write_ready(candidate, ImageSize::Thumbnail(size), &encoded)
                {
                    tracing::warn!(%error, "could not cache display artwork");
                }
            }
            (path, Some(image))
        }
    };
    if let Some(image) = &image {
        cache_preview(shared, candidate, fetched, || image.blur_hash());
    }
    Ok(Resolution::Ready {
        path,
        image: image.map(Arc::new),
    })
}

fn prepared_preview(
    shared: &Shared,
    binding: &ArtworkBinding,
    allow_external: bool,
) -> Option<Arc<crate::ArtworkPreview>> {
    binding
        .candidates
        .iter()
        .filter(|candidate| !candidate.is_external() || allow_external)
        .find_map(|candidate| cached_preview(shared, candidate, candidate.blur_hash()))
}

fn cached_preview(
    shared: &Shared,
    candidate: &Candidate,
    supplied: Option<&str>,
) -> Option<Arc<crate::ArtworkPreview>> {
    let family = FilesystemCache::candidate_family(candidate);
    let cached = {
        let mut previews = shared.previews.lock().unwrap_or_else(|p| p.into_inner());
        previews
            .iter()
            .position(|(key, hash, _)| supplied.map_or(key == &family, |supplied| hash == supplied))
            .map(|index| {
                let entry = previews.remove(index).unwrap();
                let cached = (entry.1.clone(), Arc::clone(&entry.2));
                previews.push_back(entry);
                cached
            })
    };
    let (hash, preview) = match cached {
        Some(cached) => cached,
        None => {
            let hash = supplied?;
            let image = RgbaImage::from_blur_hash(hash).ok()?;
            let png = image.encode_png().ok()?;
            (
                hash.to_owned(),
                Arc::new(crate::ArtworkPreview {
                    image,
                    png: Arc::new(png),
                }),
            )
        }
    };
    let mut previews = shared.previews.lock().unwrap_or_else(|p| p.into_inner());
    previews.retain(|(key, _, _)| key != &family);
    previews.push_back((family, hash, Arc::clone(&preview)));
    if previews.len() > 256 {
        previews.pop_front();
    }
    Some(preview)
}

fn cache_preview(
    shared: &Shared,
    candidate: &Candidate,
    refresh: bool,
    encode: impl FnOnce() -> Result<String, ArtworkError>,
) {
    let versioned = match &candidate.image {
        library::ArtworkImage::Native { tag, .. } => {
            tag.as_ref().is_some_and(|tag| !tag.is_empty())
        }
        library::ArtworkImage::Local { revision, .. }
        | library::ArtworkImage::Playlist { revision, .. } => {
            !revision.is_empty() && revision != "auto"
        }
        _ => false,
    };
    let refresh = refresh && !versioned;
    if !refresh && cached_preview(shared, candidate, candidate.blur_hash()).is_some() {
        return;
    }
    let database = shared.fetch.database();
    let result: Result<String, ArtworkError> = shared.runtime.block_on(async {
        if let Some(database) = &database {
            if !refresh && let Some(hash) = database.artwork_preview(&candidate.key).await? {
                return Ok(hash);
            }
        }
        let hash = encode()?;
        if let Some(database) = database {
            database
                .write_artwork_preview(&candidate.key, &hash, refresh)
                .await?;
        }
        Ok(hash)
    });
    match result {
        Ok(hash) => {
            cached_preview(shared, candidate, Some(&hash));
        }
        Err(error) => tracing::warn!(%error, "Could not cache artwork preview"),
    }
}

fn mark_missing(shared: &Shared, work: &Work, candidate: &Candidate) -> std::io::Result<bool> {
    let _commit = lock_cache_commit(shared);
    let state = lock_state(shared);
    if !work_is_current(&state, work) {
        return Ok(false);
    }
    drop(state);
    shared.cache.mark_missing(candidate)?;
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
        && let Resolution::Ready {
            image: Some(image), ..
        } = &resolution
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
                && !(subscriber.decode
                    && matches!(resolution, Resolution::Ready { image: None, .. }))
        })
        .map(|(_, subscriber)| subscriber.completion)
        .collect::<Vec<_>>();
    if record.has_interest() {
        // Shared callers can need pixels, original bytes, or both.
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
    async fn foreground_and_background_share_a_disk_hit() {
        let directory = tempfile::tempdir().unwrap();
        let cache = FilesystemCache::new(directory.path().to_path_buf()).unwrap();
        let request = ArtworkRequest {
            binding: ArtworkBinding::opaque(
                &sources::native_artwork_binding("source", &NativeImageRef::new("album", None))
                    .unwrap(),
            ),
            fetch_size: ImageSize::Original,
            render_size: 144,
            external: crate::ExternalPolicy::default(),
            cache_only: false,
        };
        let mut png = std::io::Cursor::new(Vec::new());
        image::DynamicImage::ImageRgba8(image::RgbaImage::from_pixel(
            1024,
            768,
            image::Rgba([40, 80, 120, 255]),
        ))
        .write_to(&mut png, image::ImageFormat::Png)
        .unwrap();
        cache
            .write_ready(
                request.binding.candidates.first().unwrap(),
                ImageSize::Original,
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
                decode: true,
                completion,
            },
        );
        let (completion, background) = oneshot::channel();
        enqueue(
            &mut state,
            request,
            RequestId(2),
            Subscriber {
                priority: JobPriority::Background,
                decode: false,
                completion,
            },
        );
        assert_eq!(state.jobs.len(), 1);
        let shared = Shared {
            runtime: Handle::current(),
            cache,
            fetch: FetchContext::new(Arc::new(Mutex::new(None))),
            cache_commit: Mutex::new(()),
            cache_resolver: Mutex::new(None),
            previews: Mutex::new(VecDeque::new()),
            state: Mutex::new(state),
            wake: Condvar::new(),
        };
        tokio::task::spawn_blocking(move || {
            let work = next_work(&shared, false, &mut false);
            let resolution = resolve(&shared, &work);
            let repeated = resolve(&shared, &work);
            let (
                Resolution::Ready {
                    image: Some(first), ..
                },
                Resolution::Ready {
                    image: Some(repeated_image),
                    ..
                },
            ) = (&resolution, &repeated)
            else {
                panic!("original and display reads must succeed")
            };
            assert_eq!(
                (repeated_image.width(), repeated_image.height()),
                (192, 144)
            );
            assert_eq!((first.width(), first.height()), (192, 144));
            assert!(
                work.request
                    .binding
                    .candidates
                    .iter()
                    .all(|candidate| candidate.blur_hash().is_none())
            );
            assert!(prepared_preview(&shared, &work.request.binding, false).is_some());
            finish(&shared, work, resolution);
        })
        .await
        .unwrap();
        let Resolution::Ready {
            image: Some(image), ..
        } = foreground.await.unwrap()
        else {
            panic!("display request must supply pixels");
        };
        assert!(image.animation_bytes().is_none());
        let Resolution::Ready { path, .. } = background.await.unwrap() else {
            panic!("original request must supply the file");
        };
        assert_eq!(std::fs::read(path).unwrap(), png.get_ref().as_slice());
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
        let available = pipeline
            .cache_image(
                ArtworkRequest::new(ArtworkBinding::opaque(br#"{"no_art":true}"#), 512),
                true,
            )
            .await;
        assert!(matches!(available, Resolution::Missing));
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
            external: crate::ExternalPolicy::default(),
            cache_only: false,
        };
        let first = enqueue(
            &mut state,
            request.clone(),
            RequestId(1),
            Subscriber {
                priority: JobPriority::Foreground,
                decode: true,
                completion: oneshot::channel().0,
            },
        );
        let second = enqueue(
            &mut state,
            request,
            RequestId(2),
            Subscriber {
                priority: JobPriority::Foreground,
                decode: true,
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
