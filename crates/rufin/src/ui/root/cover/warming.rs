use super::targets::source_warm_targets;
use super::*;

#[derive(Clone, Copy)]
struct CoverWarmSchedule {
    generation: u64,
    initial_delay_ms: u64,
}

impl Shell {
    pub(in crate::ui) fn schedule_source_route_cover_warm(
        self: &Rc<Self>,
        library: &LibrarySnapshot,
        smart_playlists: &[SmartPlaylist],
        settings: &AppSettings,
        route_metrics: InitialRouteCoverMetrics,
    ) -> (usize, usize) {
        let targets = source_warm_targets(library, smart_playlists, settings, route_metrics);
        let target_count = targets.len();
        let queued = self.schedule_warm_targets(targets);
        (target_count, queued)
    }

    pub(in crate::ui) fn warm_cover_refs_now(
        self: &Rc<Self>,
        image_refs: Vec<ImageRef>,
        fetch_size: u32,
        size: i32,
    ) {
        self.schedule_route_cover_warm_refs(image_refs, fetch_size, size, 0);
    }

    fn schedule_warm_targets(self: &Rc<Self>, targets: Vec<CoverWarmTarget>) -> usize {
        let jobs = self.cover_warm_jobs_from_targets(targets);
        let queued = jobs.len();
        if jobs.is_empty() {
            return 0;
        }

        let generation = self.next_cover_warm_generation();
        self.schedule_cover_warm_jobs(
            Rc::new(RefCell::new(jobs)),
            CoverWarmSchedule {
                generation,
                initial_delay_ms: 0,
            },
        );
        queued
    }

    pub(in crate::ui) fn prime_cover_refs_now(
        self: &Rc<Self>,
        image_refs: Vec<ImageRef>,
        fetch_size: u32,
        size: i32,
    ) {
        let decode_size = cover_decode_size(size, fetch_size);
        self.cancel_queued_warm_cover_decodes();
        let mut seen = HashSet::new();
        let jobs = image_refs
            .into_iter()
            .filter_map(|image_ref| {
                let key = self.cover_cache_key(&image_ref, fetch_size)?;
                if !seen.insert(key.clone()) {
                    return None;
                }
                Some((key, image_ref))
            })
            .collect::<Vec<_>>();
        let keep = jobs
            .iter()
            .map(|(key, _)| key.clone())
            .collect::<HashSet<_>>();
        retain_current_priority_cover_work(
            &self.state.cover_path_lookups,
            &mut self.state.cover_decode_queue.borrow_mut(),
            &keep,
        );

        for (key, image_ref) in jobs {
            if self
                .decoded_cover_for_ref(&image_ref, fetch_size, decode_size)
                .is_some()
                || self.state.cover_decodes.borrow().contains_key(&key)
            {
                continue;
            }
            self.start_cached_cover_path_lookup(CoverPathLookupRequest {
                key,
                image_ref,
                fetch_size,
                size: decode_size,
                intent: CoverPathLookupIntent::Priority,
            });
        }
    }

    fn schedule_route_cover_warm_refs(
        self: &Rc<Self>,
        image_refs: Vec<ImageRef>,
        fetch_size: u32,
        size: i32,
        initial_delay_ms: u64,
    ) {
        let jobs = self.cover_warm_jobs_from_refs(image_refs, fetch_size, size);
        if jobs.is_empty() {
            return;
        }

        let generation = self.next_cover_warm_generation();
        self.schedule_cover_warm_jobs(
            Rc::new(RefCell::new(jobs)),
            CoverWarmSchedule {
                generation,
                initial_delay_ms,
            },
        );
    }

    fn next_cover_warm_generation(&self) -> u64 {
        let generation = self.state.cover_warm_generation.get().saturating_add(1);
        self.state.cover_warm_generation.set(generation);
        generation
    }

    pub(in crate::ui) fn cancel_cover_warm(&self) {
        self.state
            .cover_warm_generation
            .set(self.state.cover_warm_generation.get().saturating_add(1));
        self.cancel_queued_warm_cover_decodes();
    }

    pub(in crate::ui) fn pause_cover_warm_for_interaction(self: &Rc<Self>) {
        let now = Instant::now();
        self.state.cover_warm_paused_until.set(Some(
            now + Duration::from_millis(COVER_WARM_SCROLL_PAUSE_MS),
        ));
        self.state.cover_visible_paused_until.set(Some(
            now + Duration::from_millis(COVER_VISIBLE_SCROLL_PAUSE_MS),
        ));
        self.schedule_cover_decode_resume();
    }

    pub(in crate::ui) fn pause_cover_warm_for_nav(self: &Rc<Self>) {
        let now = Instant::now();
        self.state.cover_warm_paused_until.set(Some(
            now + Duration::from_millis(COVER_WARM_SCROLL_PAUSE_MS),
        ));
        self.schedule_cover_decode_resume();
    }

    pub(in crate::ui) fn cover_warm_is_paused(&self) -> bool {
        self.cover_warm_pause_remaining().is_some()
    }

    pub(in crate::ui) fn cover_warm_pause_remaining(&self) -> Option<Duration> {
        let until = self.state.cover_warm_paused_until.get()?;
        let now = Instant::now();
        if now < until {
            return Some(until.saturating_duration_since(now));
        }
        self.state.cover_warm_paused_until.set(None);
        None
    }

    pub(in crate::ui) fn cover_visible_is_paused(&self) -> bool {
        self.cover_visible_pause_remaining().is_some()
    }

    pub(in crate::ui) fn cover_visible_pause_remaining(&self) -> Option<Duration> {
        let until = self.state.cover_visible_paused_until.get()?;
        let now = Instant::now();
        if now < until {
            return Some(until.saturating_duration_since(now));
        }
        self.state.cover_visible_paused_until.set(None);
        None
    }

    fn schedule_cover_decode_resume(self: &Rc<Self>) {
        if self.state.cover_decode_resume_queued.replace(true) {
            return;
        }

        let shell = Rc::clone(self);
        let delay = self
            .next_cover_decode_resume_delay()
            .unwrap_or_else(|| Duration::from_millis(COVER_VISIBLE_SCROLL_PAUSE_MS));
        glib::timeout_add_local_once(delay, move || {
            shell.state.cover_decode_resume_queued.set(false);
            shell.drain_cover_decode_queue();
            if shell.next_cover_decode_resume_delay().is_some() {
                shell.schedule_cover_decode_resume();
            }
        });
    }

    fn next_cover_decode_resume_delay(&self) -> Option<Duration> {
        match (
            self.cover_visible_pause_remaining(),
            self.cover_warm_pause_remaining(),
        ) {
            (Some(visible), Some(warm)) => Some(visible.min(warm)),
            (Some(visible), None) => Some(visible),
            (None, Some(warm)) => Some(warm),
            (None, None) => None,
        }
    }

    fn cover_warm_jobs_from_refs(
        &self,
        image_refs: Vec<ImageRef>,
        fetch_size: u32,
        size: i32,
    ) -> VecDeque<CoverWarmJob> {
        let decode_size = cover_decode_size(size, fetch_size);
        let mut seen = HashSet::new();
        let mut jobs = VecDeque::new();

        for image_ref in image_refs {
            let Some(key) = self.cover_cache_key(&image_ref, fetch_size) else {
                continue;
            };
            if !seen.insert(key.clone())
                || self
                    .decoded_cover_for_ref(&image_ref, fetch_size, decode_size)
                    .is_some()
            {
                continue;
            }
            jobs.push_back(CoverWarmJob {
                key,
                image_ref,
                fetch_size,
                size: decode_size,
            });
        }

        jobs
    }

    fn cover_warm_jobs_from_targets(
        &self,
        targets: Vec<CoverWarmTarget>,
    ) -> VecDeque<CoverWarmJob> {
        let mut seen = HashSet::new();
        let mut jobs = VecDeque::new();

        for target in targets {
            let decode_size = cover_decode_size(target.size, target.fetch_size);
            let Some(key) = self.cover_cache_key(&target.image_ref, target.fetch_size) else {
                continue;
            };
            if !seen.insert(key.clone())
                || self
                    .decoded_cover_for_ref(&target.image_ref, target.fetch_size, decode_size)
                    .is_some()
            {
                continue;
            }
            jobs.push_back(CoverWarmJob {
                key,
                image_ref: target.image_ref,
                fetch_size: target.fetch_size,
                size: decode_size,
            });
        }

        jobs
    }

    fn schedule_cover_warm_jobs(
        self: &Rc<Self>,
        jobs: Rc<RefCell<VecDeque<CoverWarmJob>>>,
        schedule: CoverWarmSchedule,
    ) {
        let shell = Rc::clone(self);
        if schedule.initial_delay_ms == 0 {
            glib::idle_add_local_once(move || {
                if shell.state.cover_warm_generation.get() == schedule.generation {
                    shell.start_cover_warm_jobs(jobs, schedule);
                }
            });
            return;
        }

        glib::timeout_add_local_once(
            Duration::from_millis(schedule.initial_delay_ms),
            move || {
                if shell.state.cover_warm_generation.get() == schedule.generation {
                    shell.start_cover_warm_jobs(jobs, schedule);
                }
            },
        );
    }

    fn start_cover_warm_jobs(
        self: &Rc<Self>,
        jobs: Rc<RefCell<VecDeque<CoverWarmJob>>>,
        schedule: CoverWarmSchedule,
    ) {
        let shell = Rc::clone(self);
        glib::timeout_add_local(Duration::from_millis(COVER_WARM_INTERVAL_MS), move || {
            let tick_started = Instant::now();
            if shell.state.cover_warm_generation.get() != schedule.generation {
                return glib::ControlFlow::Break;
            }
            if jobs.borrow().is_empty() {
                return glib::ControlFlow::Break;
            }
            if shell.cover_warm_is_paused() {
                return glib::ControlFlow::Continue;
            }

            let in_flight = shell.cover_pipeline_in_flight();
            if in_flight >= COVER_LOOKUP_LIMIT {
                return glib::ControlFlow::Continue;
            }

            let capacity = COVER_LOOKUP_LIMIT.saturating_sub(in_flight);
            let mut processed = 0;
            while processed < COVER_WARM_BATCH_SIZE.min(capacity) {
                let Some(job) = jobs.borrow_mut().pop_front() else {
                    break;
                };
                processed += 1;
                if shell.cover_job_active(&job) {
                    continue;
                }
                if !shell.decoded_cover_has_warm_capacity(job.size) {
                    continue;
                }
                shell.start_warm_cover_path_lookup(job);
            }

            let tick_ms = tick_started.elapsed().as_millis() as u64;
            if tick_ms >= SLOW_COVER_CALLBACK_MS {
                warn!(
                    processed,
                    remaining = jobs.borrow().len(),
                    in_flight,
                    tick_ms,
                    "slow cover warm tick"
                );
            }
            if jobs.borrow().is_empty() {
                glib::ControlFlow::Break
            } else {
                glib::ControlFlow::Continue
            }
        });
    }

    fn cover_pipeline_in_flight(&self) -> usize {
        self.state
            .cover_decodes
            .borrow()
            .len()
            .saturating_add(self.state.cover_path_lookups.len())
    }

    fn cover_job_active(&self, job: &CoverWarmJob) -> bool {
        self.decoded_cover_has_min_size(&job.key, job.size)
            || self.state.cover_decodes.borrow().contains_key(&job.key)
            || self.state.cover_path_lookups.contains_key(&job.key)
    }

    fn start_warm_cover_path_lookup(self: &Rc<Self>, job: CoverWarmJob) {
        self.start_cached_cover_path_lookup(CoverPathLookupRequest {
            key: job.key,
            image_ref: job.image_ref,
            fetch_size: job.fetch_size,
            size: job.size,
            intent: CoverPathLookupIntent::Warm,
        });
    }
}
