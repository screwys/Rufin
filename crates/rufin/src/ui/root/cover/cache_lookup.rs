use super::targets::startup_cover_prime_jobs;
use super::*;

impl Shell {
    fn start_cover_prime_lookup(
        self: &Rc<Self>,
        key: String,
        image_ref: ImageRef,
        fetch_size: u32,
        size: i32,
    ) {
        self.start_cached_cover_path_lookup(CoverPathLookupRequest {
            key,
            image_ref,
            fetch_size,
            size,
            intent: CoverPathLookupIntent::StartupPrime,
        });
    }

    pub(in crate::ui::root) fn begin_startup_cover_prime(self: &Rc<Self>) -> u64 {
        let generation = self
            .state
            .startup_cover_prime_generation
            .get()
            .saturating_add(1);
        self.state.startup_cover_prime_generation.set(generation);
        self.state.startup_cover_prime_pending.borrow_mut().clear();

        let jobs = startup_cover_prime_jobs(self);
        let mut pending = HashSet::new();
        for job in jobs {
            if self
                .decoded_cover_for_ref(&job.image_ref, job.fetch_size, job.size)
                .is_some()
                || self.state.cover_unavailable.borrow().contains(&job.key)
            {
                continue;
            }
            pending.insert(job.key.clone());
            self.start_cover_prime_lookup(job.key, job.image_ref, job.fetch_size, job.size);
        }

        let pending_count = pending.len();
        *self.state.startup_cover_prime_pending.borrow_mut() = pending;
        if pending_count > 0 {
            info!(covers = pending_count, "started startup cover prime");
        }
        generation
    }

    pub(in crate::ui::root) fn finish_startup_cover_prime_gate(&self) {
        self.state.startup_cover_prime_generation.set(
            self.state
                .startup_cover_prime_generation
                .get()
                .saturating_add(1),
        );
        self.state.startup_cover_prime_pending.borrow_mut().clear();
    }

    pub(in crate::ui::root) fn startup_cover_prime_pending_count(
        &self,
        generation: Option<u64>,
    ) -> usize {
        match generation {
            Some(generation) if self.state.startup_cover_prime_generation.get() == generation => {
                self.state.startup_cover_prime_pending.borrow().len()
            }
            Some(_) => 0,
            None => 1,
        }
    }

    pub(in crate::ui::root) fn begin_first_run_cover_prime(self: &Rc<Self>) -> Option<u64> {
        let generation = self
            .state
            .first_run_cover_prime_generation
            .get()
            .saturating_add(1);
        self.state.first_run_cover_prime_generation.set(generation);
        self.state
            .first_run_cover_prime_pending
            .borrow_mut()
            .clear();

        let jobs = self.first_run_cover_prime_jobs();
        if jobs.is_empty() {
            return None;
        }

        let mut pending = HashSet::new();
        for job in jobs {
            if self
                .decoded_cover_for_ref(&job.image_ref, job.fetch_size, job.size)
                .is_some()
                || self.state.cover_unavailable.borrow().contains(&job.key)
            {
                continue;
            }
            pending.insert(job.key.clone());
            self.start_cover_prime_lookup(job.key, job.image_ref, job.fetch_size, job.size);
        }

        if pending.is_empty() {
            return None;
        }
        let pending_count = pending.len();
        *self.state.first_run_cover_prime_pending.borrow_mut() = pending;
        info!(covers = pending_count, "started first-run cover prime");
        Some(generation)
    }

    pub(in crate::ui::root) fn first_run_cover_prime_current(&self, generation: u64) -> bool {
        self.state.first_run_cover_prime_generation.get() == generation
    }

    pub(in crate::ui::root) fn first_run_cover_prime_pending_count(&self) -> usize {
        self.state.first_run_cover_prime_pending.borrow().len()
    }

    pub(in crate::ui::root) fn finish_first_run_cover_prime_gate(&self) {
        self.state.first_run_cover_prime_generation.set(
            self.state
                .first_run_cover_prime_generation
                .get()
                .saturating_add(1),
        );
        self.state
            .first_run_cover_prime_pending
            .borrow_mut()
            .clear();
    }

    fn first_run_cover_prime_jobs(&self) -> Vec<FirstRunCoverPrimeJob> {
        let image_refs = first_run_cover_prime_refs(&self.state.library.borrow());
        let mut seen = HashSet::new();
        let mut jobs = Vec::new();
        for image_ref in image_refs {
            let Some(key) = self.cover_cache_key(&image_ref, GRID_COVER_SIZE) else {
                continue;
            };
            if !seen.insert(key.clone()) {
                continue;
            }
            jobs.push(FirstRunCoverPrimeJob {
                key,
                image_ref,
                fetch_size: GRID_COVER_SIZE,
                size: GRID_COVER_SIZE as i32,
            });
        }
        jobs
    }

    pub(in crate::ui) fn current_playback_cached_artwork_path(
        &self,
        entry: &QueueEntry,
        preferred_size: u32,
    ) -> Option<PlaybackArtworkPath> {
        let server_id = self.current_playback_server_id()?;
        let image_ref = entry.image_ref.as_ref()?;
        let cache = self.state.cover_path_cache.borrow();
        playback_artwork_path_from_lookup(&server_id, image_ref, preferred_size, |key| {
            cache.get(key).cloned()
        })
    }

    pub(in crate::ui) fn current_playback_art_key_matches(
        &self,
        key: &str,
        preferred_size: u32,
    ) -> bool {
        let Some(server_id) = self.current_playback_server_id() else {
            return false;
        };
        self.state
            .player
            .borrow()
            .current
            .as_ref()
            .and_then(|entry| entry.image_ref.as_ref())
            .is_some_and(|image_ref| {
                playback_artwork_key_matches(&server_id, image_ref, preferred_size, key)
            })
    }

    fn current_playback_server_id(&self) -> Option<ServerId> {
        self.state
            .queue
            .borrow()
            .as_ref()
            .map(|queue| queue.server_id.clone())
            .or_else(|| {
                self.state
                    .library
                    .borrow()
                    .server
                    .as_ref()
                    .map(|server| server.id.clone())
            })
    }

    pub(in crate::ui) fn cover_cache_key(&self, image_ref: &ImageRef, size: u32) -> Option<String> {
        let server = self.state.library.borrow().server.clone()?;
        if server.provider == "fake" {
            return None;
        }
        if external_metadata::is_external_image_ref(image_ref)
            && !external_metadata::cached_refs_enabled(&self.state.settings.borrow())
        {
            return None;
        }
        Some(image_cache_key(
            &server.id,
            &image_ref.item_id,
            image_ref.tag.as_deref().unwrap_or(IMAGE_TAG_UNTAGGED),
            size,
        ))
    }
    pub(in crate::ui) fn current_playback_cover_cache_key(
        &self,
        image_ref: &ImageRef,
        size: u32,
    ) -> Option<String> {
        let server_id = self.current_playback_server_id()?;
        if self
            .state
            .library
            .borrow()
            .server
            .as_ref()
            .is_some_and(|server| server.provider == "fake")
        {
            return None;
        }
        if external_metadata::is_external_image_ref(image_ref)
            && !external_metadata::cached_refs_enabled(&self.state.settings.borrow())
        {
            return None;
        }
        Some(image_cache_key(
            &server_id,
            &image_ref.item_id,
            image_ref.tag.as_deref().unwrap_or(IMAGE_TAG_UNTAGGED),
            size,
        ))
    }
    pub(in crate::ui) fn cover_cache_candidate_keys(
        &self,
        image_ref: &ImageRef,
        preferred_size: u32,
    ) -> Vec<String> {
        decoded_cover_candidate_sizes(preferred_size)
            .into_iter()
            .filter_map(|size| self.cover_cache_key(image_ref, size))
            .collect()
    }
    pub(in crate::ui) fn decoded_cover_for_ref(
        &self,
        image_ref: &ImageRef,
        preferred_size: u32,
        min_size: i32,
    ) -> Option<(String, Pixbuf)> {
        for size in decoded_cover_candidate_sizes(preferred_size) {
            let Some(key) = self.cover_cache_key(image_ref, size) else {
                continue;
            };
            if let Some(cover) = self.cloned_decoded_cover(&key, min_size) {
                return Some((key, cover.pixbuf));
            }
        }
        None
    }
    pub(in crate::ui::root::cover) fn start_cached_cover_path_lookup(
        self: &Rc<Self>,
        request: CoverPathLookupRequest,
    ) {
        if request.intent != CoverPathLookupIntent::Warm {
            self.state.cover_visible_requests.record(request.clone());
        }
        let CoverPathLookupRequest {
            key,
            image_ref,
            fetch_size,
            size,
            intent,
        } = request;
        if self.state.cover_unavailable.borrow().contains(&key) {
            self.apply_cover_unavailable(&key);
            return;
        }
        let mut candidate_keys = self.cover_cache_candidate_keys(&image_ref, fetch_size);
        if !candidate_keys.iter().any(|candidate| candidate == &key) {
            candidate_keys.insert(0, key.clone());
        }
        let fast_path = if matches!(
            intent,
            CoverPathLookupIntent::Priority | CoverPathLookupIntent::StartupPrime
        ) {
            cover_candidate_path(
                &candidate_keys,
                |candidate_key| {
                    self.state
                        .cover_path_cache
                        .borrow()
                        .get(candidate_key)
                        .cloned()
                },
                |candidate_key| self.controller.cached_cover_path_for_key(candidate_key),
            )
        } else {
            cached_cover_candidate_path(&candidate_keys, |candidate_key| {
                self.state
                    .cover_path_cache
                    .borrow()
                    .get(candidate_key)
                    .cloned()
            })
        };
        if let Some(path) = fast_path {
            self.finish_cached_cover_path_lookup(
                key,
                image_ref,
                fetch_size,
                size,
                intent,
                Some(path),
            );
            return;
        }
        let should_start = self.state.cover_path_lookups.record(key.clone(), intent);
        if !should_start {
            return;
        }

        let shell = Rc::clone(self);
        let controller = self.controller.clone();
        let image_ref_for_lookup = image_ref.clone();
        glib::spawn_future_local(async move {
            let path = gtk::gio::spawn_blocking(move || {
                cached_cover_path_for_lookup(
                    &candidate_keys,
                    |key| controller.cached_cover_path_for_key(key),
                    || controller.cached_cover_path(&image_ref_for_lookup, fetch_size),
                )
            })
            .await
            .ok()
            .flatten();
            let Some(intent) = shell.state.cover_path_lookups.remove(&key) else {
                return;
            };
            let finish_started = Instant::now();
            let has_path = path.is_some();
            shell.finish_cached_cover_path_lookup(key, image_ref, fetch_size, size, intent, path);
            let finish_ms = finish_started.elapsed().as_millis() as u64;
            if finish_ms >= SLOW_COVER_CALLBACK_MS {
                warn!(
                    ?intent,
                    has_path, finish_ms, "slow cached cover lookup finish"
                );
            }
        });
    }
    pub(in crate::ui::root::cover) fn finish_cached_cover_path_lookup(
        self: &Rc<Self>,
        key: String,
        image_ref: ImageRef,
        fetch_size: u32,
        size: i32,
        intent: CoverPathLookupIntent,
        path: Option<PathBuf>,
    ) {
        if let Some(path) = path.as_ref() {
            self.state
                .cover_path_cache
                .borrow_mut()
                .insert(key.clone(), path.clone());
        }
        match intent {
            CoverPathLookupIntent::Warm => {
                if let Some(path) = path {
                    self.start_cover_decode_from_path(key, path, size, CoverDecodePriority::Warm);
                } else if self.should_fetch_warm_cover(&key, &image_ref) {
                    self.request_cover_for_key(key, image_ref, fetch_size);
                }
            }
            CoverPathLookupIntent::Priority => {
                if let Some(path) = path {
                    self.state.cover_unavailable.borrow_mut().remove(&key);
                    self.start_cover_decode_from_path(
                        key,
                        path,
                        size,
                        CoverDecodePriority::Visible,
                    );
                } else if self.should_fetch_cover(&key, &image_ref) {
                    self.request_cover_for_key(key, image_ref, fetch_size);
                } else {
                    self.apply_cover_unavailable(&key);
                }
            }
            CoverPathLookupIntent::StartupPrime => {
                if let Some(path) = path {
                    self.state.cover_unavailable.borrow_mut().remove(&key);
                    self.start_cover_decode_from_path(
                        key.clone(),
                        path,
                        size,
                        CoverDecodePriority::Visible,
                    );
                } else if self.should_fetch_cover(&key, &image_ref) {
                    self.request_cover_for_key(key, image_ref, fetch_size);
                } else {
                    self.state
                        .cover_unavailable
                        .borrow_mut()
                        .insert(key.clone());
                    self.remove_cover_prime_pending(&key);
                }
            }
            CoverPathLookupIntent::Visible => {
                self.finish_visible_cover_path_lookup(key, image_ref, fetch_size, size, path);
            }
        }
    }
    fn finish_visible_cover_path_lookup(
        self: &Rc<Self>,
        key: String,
        image_ref: ImageRef,
        fetch_size: u32,
        size: i32,
        path: Option<PathBuf>,
    ) {
        let Some(path) = path else {
            if self.should_fetch_cover(&key, &image_ref) {
                self.mark_cover_request_state(&key, CoverRequestState::Fetching);
                self.request_cover_for_key(key, image_ref, fetch_size);
                return;
            }
            self.apply_cover_unavailable(&key);
            return;
        };
        self.state.cover_unavailable.borrow_mut().remove(&key);

        if !self.cover_binding_has_live(&key) {
            self.state.cover_visible_requests.remove(&key);
            return;
        }

        let size = self
            .pending_cover_size(&key)
            .map(|pending_size| {
                let fetch_size = cover_size_from_cache_key(&key).unwrap_or(size).max(1) as u32;
                cover_decode_size(pending_size, fetch_size).max(size)
            })
            .unwrap_or(size);
        self.mark_cover_request_state(&key, CoverRequestState::Decoding);
        self.start_cover_decode_from_path(key, path, size, CoverDecodePriority::Visible);
    }
    fn should_fetch_cover(&self, key: &str, image_ref: &ImageRef) -> bool {
        let provider = self
            .state
            .library
            .borrow()
            .server
            .as_ref()
            .map(|server| server.provider.clone());
        let unavailable = self.state.cover_unavailable.borrow().contains(key);
        visible_cover_cache_miss_action(provider.as_deref(), image_ref, unavailable, false)
            == VisibleCoverCacheMissAction::Fetch
    }
    fn should_fetch_warm_cover(&self, key: &str, image_ref: &ImageRef) -> bool {
        let provider = self
            .state
            .library
            .borrow()
            .server
            .as_ref()
            .map(|server| server.provider.clone());
        let unavailable = self.state.cover_unavailable.borrow().contains(key);
        warm_cover_cache_miss_action(provider.as_deref(), image_ref, unavailable, false)
            == VisibleCoverCacheMissAction::Fetch
    }
    fn request_cover_for_key(&self, key: String, image_ref: ImageRef, fetch_size: u32) {
        let enqueue_started = Instant::now();
        let requested = self
            .controller
            .request_cover_for_key(key.clone(), image_ref, fetch_size);
        let enqueue_ms = enqueue_started.elapsed().as_millis() as u64;
        if enqueue_ms >= SLOW_COVER_CALLBACK_MS {
            warn!(enqueue_ms, "slow cover fetch enqueue");
        }
        if requested {
            self.state.cover_fetches.borrow_mut().insert(key);
        }
    }
    pub(in crate::ui) fn apply_cover_ready(self: &Rc<Self>, key: &str, path: &Path) {
        let apply_started = Instant::now();
        self.state.cover_fetches.borrow_mut().remove(key);
        self.state.cover_unavailable.borrow_mut().remove(key);
        self.state
            .cover_path_cache
            .borrow_mut()
            .insert(key.to_string(), path.to_path_buf());
        let size = self
            .pending_cover_size(key)
            .unwrap_or(GRID_COVER_SIZE as i32);
        if let Some(cover) = self.cloned_decoded_cover(key, size) {
            self.touch_visible_decoded_cover(key);
            self.finish_cover_ready_state(key);
            let bindings = self.take_live_cover_bindings(key);
            let bindings_count = bindings.len();
            let bind_started = Instant::now();
            apply_pixbuf_to_bindings(bindings, cover.pixbuf);
            let bind_ms = bind_started.elapsed().as_millis() as u64;
            let total_ms = apply_started.elapsed().as_millis() as u64;
            if total_ms >= SLOW_COVER_CALLBACK_MS || bind_ms >= SLOW_COVER_CALLBACK_MS {
                warn!(
                    bindings = bindings_count,
                    bind_ms,
                    total_ms,
                    cached = true,
                    "slow cover ready apply"
                );
            }
            return;
        }
        self.mark_cover_request_state(key, CoverRequestState::Decoding);
        self.start_cover_decode_from_path(
            key.to_string(),
            path.to_path_buf(),
            size,
            CoverDecodePriority::Visible,
        );
        let total_ms = apply_started.elapsed().as_millis() as u64;
        if total_ms >= SLOW_COVER_CALLBACK_MS {
            warn!(total_ms, cached = false, "slow cover ready apply");
        }
    }
    pub(in crate::ui) fn apply_cover_unavailable(self: &Rc<Self>, key: &str) {
        self.state.cover_fetches.borrow_mut().remove(key);
        self.finish_cover_missing_state(key);
        self.state
            .cover_unavailable
            .borrow_mut()
            .insert(key.to_string());
        self.state.cover_path_cache.borrow_mut().remove(key);

        let bindings = self.take_live_cover_bindings(key);
        for binding in bindings {
            if !binding.clear_on_failure {
                continue;
            }
            if let Some(tile) = binding.tile.upgrade() {
                tile.clear_image_if_current(binding.generation);
            }
        }
    }
    pub(in crate::ui) fn clear_cover_unavailable(&self) {
        self.state.cover_unavailable.borrow_mut().clear();
    }
    pub(in crate::ui) fn apply_cover_deferred(self: &Rc<Self>, key: &str) {
        self.state.cover_fetches.borrow_mut().remove(key);
        self.mark_cover_request_state(key, CoverRequestState::Deferred);
        let request = self.state.cover_visible_requests.request(key);
        if request.is_none() && !self.cover_binding_has_live(key) {
            return;
        }
        let Some(request) = request else {
            return;
        };
        let shell = Rc::clone(self);
        glib::timeout_add_local_once(Duration::from_millis(500), move || {
            shell.start_cached_cover_path_lookup(request);
        });
    }
    pub(in crate::ui::root::cover) fn mark_cover_request_state(
        &self,
        key: &str,
        state: CoverRequestState,
    ) {
        self.state.cover_visible_requests.mark(key, state);
    }
    pub(in crate::ui::root::cover) fn finish_cover_ready_state(&self, key: &str) {
        self.mark_cover_request_state(key, CoverRequestState::Ready);
        self.state.cover_visible_requests.remove(key);
        self.remove_cover_prime_pending(key);
    }
    pub(in crate::ui::root::cover) fn finish_cover_missing_state(&self, key: &str) {
        self.mark_cover_request_state(key, CoverRequestState::FinalMissing);
        self.state.cover_visible_requests.remove(key);
        self.remove_cover_prime_pending(key);
    }
    pub(in crate::ui::root::cover) fn remove_cover_prime_pending(&self, key: &str) {
        self.state
            .startup_cover_prime_pending
            .borrow_mut()
            .remove(key);
        self.state
            .first_run_cover_prime_pending
            .borrow_mut()
            .remove(key);
    }
    pub(in crate::ui) fn reconcile_startup_cover_prime_pending(&self) {
        let stale = self.stale_cover_prime_pending_keys(
            &self
                .state
                .startup_cover_prime_pending
                .borrow()
                .iter()
                .cloned()
                .collect::<Vec<_>>(),
        );
        self.remove_stale_cover_prime_pending(stale);
    }
    pub(in crate::ui) fn reconcile_prime_pending(&self) {
        let stale = self.stale_cover_prime_pending_keys(
            &self
                .state
                .first_run_cover_prime_pending
                .borrow()
                .iter()
                .cloned()
                .collect::<Vec<_>>(),
        );
        self.remove_stale_cover_prime_pending(stale);
    }
    fn stale_cover_prime_pending_keys(&self, keys: &[String]) -> Vec<String> {
        keys.iter()
            .filter(|key| !self.prime_key_wait(key))
            .cloned()
            .collect()
    }
    fn prime_key_wait(&self, key: &str) -> bool {
        startup_prime_wait(
            self.decoded_cover_has_min_size(key, cover_size_from_cache_key(key).unwrap_or(1)),
            self.state.cover_unavailable.borrow().contains(key),
            self.state.cover_path_lookups.contains_key(key),
            self.state.cover_fetches.borrow().contains(key),
            self.state.cover_decodes.borrow().contains_key(key),
            self.state
                .cover_decode_queue
                .borrow()
                .iter()
                .any(|job| job.key == key),
        )
    }
    fn remove_stale_cover_prime_pending(&self, keys: Vec<String>) {
        if keys.is_empty() {
            return;
        }
        let mut startup_pending = self.state.startup_cover_prime_pending.borrow_mut();
        let mut first_run_pending = self.state.first_run_cover_prime_pending.borrow_mut();
        let mut unavailable = self.state.cover_unavailable.borrow_mut();
        for key in keys {
            startup_pending.remove(&key);
            first_run_pending.remove(&key);
            unavailable.insert(key.clone());
        }
    }
    pub(in crate::ui) fn start_cover_decode_from_path(
        self: &Rc<Self>,
        key: String,
        path: PathBuf,
        size: i32,
        priority: CoverDecodePriority,
    ) {
        if self.apply_decoded_cover_if_available(&key, size, priority) {
            return;
        }
        if priority == CoverDecodePriority::Warm && !self.decoded_cover_has_warm_capacity(size) {
            return;
        }

        if self.state.cover_decodes.borrow().contains_key(&key) {
            return;
        }

        {
            let requires_live_binding =
                priority == CoverDecodePriority::Visible && self.cover_binding_has_live(&key);
            let priority = cover_decode_priority_for_interaction(
                priority,
                requires_live_binding,
                self.cover_warm_is_paused(),
            );
            let mut queue = self.state.cover_decode_queue.borrow_mut();
            if let Some(position) = queue.iter().position(|job| job.key == key) {
                let Some(mut job) = queue.remove(position) else {
                    return;
                };
                job.size = job.size.max(size);
                job.requires_live_binding |= requires_live_binding;
                job.priority = if job.priority == CoverDecodePriority::Visible
                    || priority == CoverDecodePriority::Visible
                {
                    CoverDecodePriority::Visible
                } else {
                    CoverDecodePriority::Warm
                };
                queue_cover_decode_job(&mut queue, job);
                drop(queue);
                self.drain_cover_decode_queue();
                return;
            }

            let job = CoverDecodeJob {
                key,
                path,
                size,
                priority,
                requires_live_binding,
            };
            queue_cover_decode_job(&mut queue, job);
        }

        self.drain_cover_decode_queue();
    }
    pub(in crate::ui) fn apply_decoded_cover_if_available(
        &self,
        key: &str,
        min_size: i32,
        priority: CoverDecodePriority,
    ) -> bool {
        let Some(cover) = self.cloned_decoded_cover(key, min_size) else {
            return false;
        };
        self.touch_decoded_cover(key, priority);
        self.finish_cover_ready_state(key);
        let bindings = self.take_live_cover_bindings(key);
        apply_pixbuf_to_bindings(bindings, cover.pixbuf);
        true
    }
}

pub(in crate::ui) fn startup_prime_wait(
    decoded: bool,
    unavailable: bool,
    path_lookup: bool,
    fetch: bool,
    active_decode: bool,
    queued_decode: bool,
) -> bool {
    !decoded && !unavailable && (path_lookup || fetch || active_decode || queued_decode)
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(in crate::ui) enum VisibleCoverCacheMissAction {
    Fetch,
    FinalMissing,
}

pub(in crate::ui) fn visible_cover_cache_miss_action(
    provider: Option<&str>,
    image_ref: &ImageRef,
    unavailable: bool,
    external_known_missing: bool,
) -> VisibleCoverCacheMissAction {
    if unavailable || external_known_missing {
        return VisibleCoverCacheMissAction::FinalMissing;
    }
    if image_ref.item_id.starts_with("local:cover:") {
        return if provider == Some(source_local::LOCAL_PROVIDER_ID) {
            VisibleCoverCacheMissAction::Fetch
        } else {
            VisibleCoverCacheMissAction::FinalMissing
        };
    }
    VisibleCoverCacheMissAction::Fetch
}

pub(in crate::ui) fn warm_cover_cache_miss_action(
    provider: Option<&str>,
    image_ref: &ImageRef,
    unavailable: bool,
    external_known_missing: bool,
) -> VisibleCoverCacheMissAction {
    if external_metadata::is_external_image_ref(image_ref) {
        return VisibleCoverCacheMissAction::FinalMissing;
    }
    visible_cover_cache_miss_action(provider, image_ref, unavailable, external_known_missing)
}

pub(in crate::ui) fn cached_cover_candidate_path(
    candidate_keys: &[String],
    mut key_lookup: impl FnMut(&str) -> Option<PathBuf>,
) -> Option<PathBuf> {
    candidate_keys.iter().find_map(|key| key_lookup(key))
}

pub(in crate::ui) fn cover_candidate_path(
    candidate_keys: &[String],
    mut memory_lookup: impl FnMut(&str) -> Option<PathBuf>,
    mut disk_lookup: impl FnMut(&str) -> Option<PathBuf>,
) -> Option<PathBuf> {
    candidate_keys
        .iter()
        .find_map(|key| memory_lookup(key).or_else(|| disk_lookup(key)))
}

pub(in crate::ui) fn cached_cover_path_for_lookup(
    candidate_keys: &[String],
    key_lookup: impl FnMut(&str) -> Option<PathBuf>,
    fallback_lookup: impl FnOnce() -> Option<PathBuf>,
) -> Option<PathBuf> {
    cached_cover_candidate_path(candidate_keys, key_lookup).or_else(fallback_lookup)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cover_use_miss() {
        let fallback = PathBuf::from("/tmp/rufin-cached-cover.jpg");
        let keys = vec!["cover-96".to_string(), "cover-256".to_string()];

        assert_eq!(
            cached_cover_path_for_lookup(&keys, |_| None, || Some(fallback.clone())),
            Some(fallback)
        );
    }

    #[test]
    fn cover_path_files() {
        let key_path = PathBuf::from("/tmp/rufin-key-cover.jpg");
        let fallback = PathBuf::from("/tmp/rufin-cached-cover.jpg");
        let keys = vec!["cover-96".to_string(), "cover-256".to_string()];

        assert_eq!(
            cached_cover_path_for_lookup(
                &keys,
                |key| (key == "cover-256").then(|| key_path.clone()),
                || Some(fallback)
            ),
            Some(key_path)
        );
    }

    #[test]
    fn cover_use_fallback() {
        let disk_path = PathBuf::from("/tmp/rufin-disk-candidate.jpg");
        let keys = vec!["cover-96".to_string(), "cover-256".to_string()];

        assert_eq!(
            cover_candidate_path(
                &keys,
                |_| None,
                |key| (key == "cover-96").then(|| disk_path.clone())
            ),
            Some(disk_path)
        );
    }

    #[test]
    fn cover_wait_work() {
        assert!(startup_prime_wait(false, false, true, false, false, false));
        assert!(startup_prime_wait(false, false, false, true, false, false));
        assert!(startup_prime_wait(false, false, false, false, true, false));
        assert!(startup_prime_wait(false, false, false, false, false, true));
        assert!(!startup_prime_wait(true, false, true, true, false, false));
        assert!(!startup_prime_wait(false, true, true, true, false, false));
        assert!(!startup_prime_wait(
            false, false, false, false, false, false
        ));
    }

    #[test]
    fn cover_fetch_local() {
        let local_cover = ImageRef::new("local:cover:file%3A%2F%2Fcover.jpg", None);

        assert_eq!(
            visible_cover_cache_miss_action(
                Some(source_local::LOCAL_PROVIDER_ID),
                &local_cover,
                false,
                false
            ),
            VisibleCoverCacheMissAction::Fetch
        );
    }

    #[test]
    fn cover_fetch_missing() {
        let provider_cover = ImageRef::new("album-1", None);

        assert_eq!(
            visible_cover_cache_miss_action(Some("jellyfin"), &provider_cover, false, false),
            VisibleCoverCacheMissAction::Fetch
        );
    }

    #[test]
    fn cover_stale_source() {
        let local_cover = ImageRef::new("local:cover:embedded%3A%2Fmusic%2Ftrack.flac", None);

        assert_eq!(
            visible_cover_cache_miss_action(Some("jellyfin"), &local_cover, false, false),
            VisibleCoverCacheMissAction::FinalMissing
        );
    }

    #[test]
    fn cover_missing_final() {
        let external_cover = ImageRef::new("external:album:artist:album", None);

        assert_eq!(
            visible_cover_cache_miss_action(Some("jellyfin"), &external_cover, false, true),
            VisibleCoverCacheMissAction::FinalMissing
        );
    }

    #[test]
    fn warm_cover_fetches_provider() {
        let provider_cover = ImageRef::new("album-1", None);

        assert_eq!(
            warm_cover_cache_miss_action(Some("jellyfin"), &provider_cover, false, false),
            VisibleCoverCacheMissAction::Fetch
        );
    }

    #[test]
    fn warm_cover_skips_external() {
        let external_cover = ImageRef::new("external:album:artist:album", None);

        assert_eq!(
            warm_cover_cache_miss_action(Some("jellyfin"), &external_cover, false, false),
            VisibleCoverCacheMissAction::FinalMissing
        );
    }
}
