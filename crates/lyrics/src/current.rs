//! One current lyrics document and its user operations.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use async_channel::Sender;
use library::{Database, ReadCancellation};
use playback::{CurrentMedia, CurrentMediaId};
use serde::{Deserialize, Serialize};
use sources::{Source, SourceId};
use tokio::sync::Semaphore;
use tokio::task::JoinHandle;
use tracing::{debug, warn};

use crate::lyrics::{
    LyricsLookup, LyricsPlan, cached_lyrics_allowed, embedded_lyrics_from_audio,
    external_best_lyrics, local_sidecar_lyrics, lyrics_from_edited_text, lyrics_from_native,
    lyrics_with_displayable_content,
};
use crate::{
    CurrentLyrics, CurrentLyricsContent, LocalLyricsInput, LyricsBundle, LyricsDocument,
    LyricsEvent, LyricsOrigin, LyricsQuery, LyricsRole, LyricsSearchResult, Settings,
    lyrics_from_search_result, lyrics_to_lrc_text, save_current_lyrics, search_lyrics,
};

const LYRICS_CACHE_PAYLOAD_VERSION: u32 = 4;

#[derive(Clone)]
pub struct LyricsContext {
    pub media: Arc<CurrentMedia>,
    pub input_digest: [u8; 32],
    pub source: Arc<dyn Fn(&SourceId) -> Option<Arc<Source>> + Send + Sync>,
    pub database: Database,
}

impl LyricsContext {
    async fn source(&self) -> Option<Arc<Source>> {
        let source_id = match library::source_entity_parts(&self.media.media_uri) {
            Some((source, kind, _)) if kind == "track" => source,
            Some(_) => return None,
            None => SourceId::new(
                self.database
                    .track_row_by_uri(&self.media.media_uri, &ReadCancellation::new())
                    .await
                    .ok()??
                    .source_id,
            ),
        };
        let source = Arc::clone(&self.source);
        tokio::task::spawn_blocking(move || source(&source_id))
            .await
            .ok()
            .flatten()
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct DocumentKey {
    media_uri: String,
}

impl DocumentKey {
    fn for_context(context: &LyricsContext) -> Self {
        Self {
            media_uri: context.media.media_uri.clone(),
        }
    }

    fn cache_key(&self, plan: &LyricsPlan) -> (String, String, String) {
        self.cache_key_for(
            if plan.prefers_translations() {
                LyricsRole::Translation
            } else {
                LyricsRole::Original
            },
            if plan.prefers_translations() {
                plan.preferred_translation_language()
            } else {
                ""
            },
        )
    }

    fn cache_key_for(&self, role: LyricsRole, language: &str) -> (String, String, String) {
        (
            role.key().to_string(),
            if role == LyricsRole::Translation {
                crate::normalize_language_tag(language).unwrap_or_default()
            } else {
                String::new()
            },
            String::new(),
        )
    }
}

struct CurrentDocument {
    context: LyricsContext,
    key: DocumentKey,
    document: Option<Arc<LyricsDocument>>,
    pronunciation: Option<Arc<LyricsDocument>>,
    bundle: Option<Arc<LyricsBundle>>,
    request: u64,
    loading: bool,
    automatic_attempted: bool,
    writable: bool,
}

#[derive(Clone)]
struct PendingSearch {
    request: u64,
    key: DocumentKey,
    query: LyricsQuery,
}

struct State {
    settings: Settings,
    private_mode: bool,
    current: Option<CurrentDocument>,
    current_cancelled: Option<Arc<AtomicBool>>,
    current_task: Option<JoinHandle<()>>,
    search: Option<PendingSearch>,
}

struct CurrentResolution {
    context: LyricsContext,
    lookup: LyricsLookup,
    local: Option<LocalLyricsInput>,
    cue_track: bool,
    plan: LyricsPlan,
}

struct LyricsWriteTarget {
    context: LyricsContext,
    content: String,
    sidecar: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum LyricsAuthority {
    Source,
    External,
}

#[derive(Deserialize, Serialize)]
struct CachedBundle {
    version: u32,
    bundle: LyricsBundle,
}

pub struct LyricsService {
    dictionary_directory: PathBuf,
    dictionary_status: Mutex<crate::JapaneseDictionaryStatus>,
    database: Database,
    runtime: tokio::runtime::Handle,
    events: Sender<LyricsEvent>,
    state: Mutex<State>,
    next_request: AtomicU64,
    search_lane: Arc<Semaphore>,
    write_lane: Arc<Semaphore>,
}

#[derive(Clone)]
pub struct LyricsHandle {
    service: Arc<LyricsService>,
}

impl LyricsService {
    pub fn new(
        database: Database,
        runtime: tokio::runtime::Handle,
        settings: Settings,
        private_mode: bool,
        events: Sender<LyricsEvent>,
        dictionary_directory: PathBuf,
    ) -> Arc<Self> {
        Arc::new(Self {
            dictionary_directory,
            dictionary_status: Mutex::new(crate::JapaneseDictionaryStatus::Idle),
            database,
            runtime,
            events,
            state: Mutex::new(State {
                settings,
                private_mode,
                current: None,
                current_cancelled: None,
                current_task: None,
                search: None,
            }),
            next_request: AtomicU64::new(1),
            search_lane: Arc::new(Semaphore::new(1)),
            write_lane: Arc::new(Semaphore::new(1)),
        })
    }

    pub fn handle(self: &Arc<Self>) -> LyricsHandle {
        LyricsHandle {
            service: Arc::clone(self),
        }
    }

    pub fn set_current(self: &Arc<Self>, context: Option<LyricsContext>) {
        let Some(context) = context else {
            self.clear();
            return;
        };
        let key = DocumentKey::for_context(&context);
        let (event, write_check) = {
            let mut state = self
                .state
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            if let Some(current) = state.current.as_mut().filter(|current| current.key == key) {
                let media_changed = current.context.media.id != context.media.id;
                let write_check = media_changed.then(|| (key.clone(), context.clone()));
                current.context = context;
                (media_changed.then(|| current_event(current)), write_check)
            } else {
                cancel_current_work(&mut state);
                let request = self.next_request.fetch_add(1, Ordering::AcqRel);
                let write_check = (key.clone(), context.clone());
                let current = CurrentDocument {
                    context,
                    key,
                    document: None,
                    pronunciation: None,
                    bundle: None,
                    request,
                    loading: false,
                    automatic_attempted: false,
                    writable: false,
                };
                let event = Some(current_event(&current));
                state.current = Some(current);
                state.search = None;
                (event, Some(write_check))
            }
        };
        if let Some(event) = event {
            self.publish(event);
        }
        if let Some((key, context)) = write_check {
            self.check_write_access(key, context);
        }
    }

    fn check_write_access(self: &Arc<Self>, key: DocumentKey, context: LyricsContext) {
        let service = Arc::clone(self);
        self.runtime.spawn(async move {
            let sidecar = service
                .state
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .settings
                .save_lyrics_as_sidecar;
            let writable = match context.source().await {
                Some(source) => {
                    source
                        .lyrics_writable(&context.database, &key.media_uri, sidecar)
                        .await
                }
                None => false,
            };
            let event = {
                let mut state = service
                    .state
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner());
                if state.settings.save_lyrics_as_sidecar != sidecar {
                    return;
                }
                let Some(current) = state.current.as_mut().filter(|current| current.key == key)
                else {
                    return;
                };
                if current.writable == writable {
                    return;
                }
                current.writable = writable;
                current_event(current)
            };
            service.publish(event);
        });
    }

    fn refresh_write_access(self: &Arc<Self>) {
        let current = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .current
            .as_ref()
            .map(|current| (current.key.clone(), current.context.clone()));
        if let Some((key, context)) = current {
            self.check_write_access(key, context);
        }
    }

    pub fn settings_changed(self: &Arc<Self>, settings: Settings, private_mode: bool) {
        let restart = {
            let mut state = self
                .state
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            let current = state.current.as_ref().map(|current| {
                (
                    current.context.media.id.clone(),
                    current.key.media_uri.clone(),
                    current.automatic_attempted || current.loading || current.bundle.is_some(),
                    current.loading,
                    current.bundle.is_some(),
                    current
                        .document
                        .as_deref()
                        .is_some_and(LyricsDocument::has_word_timing),
                )
            });
            let selection_changed = lyrics_selection_changed(&state.settings, &settings);
            let karaoke_enabled = !state.settings.karaoke_mode && settings.karaoke_mode;
            let acquisition_changed = current.as_ref().is_some_and(|(_, track_id, ..)| {
                lyrics_acquisition_changed(&state.settings, &settings, track_id)
            });
            let settings_changed = selection_changed || acquisition_changed || karaoke_enabled;
            let private_changed = state.private_mode != private_mode;
            state.settings = settings;
            state.private_mode = private_mode;
            if !settings_changed && !private_changed {
                return;
            }
            state.search = None;
            let Some((media_id, track_id, attempted, loading, has_document, has_word_timing)) =
                current
            else {
                return;
            };
            let restart = attempted
                && (selection_changed
                    || karaoke_enabled && !has_word_timing
                    || acquisition_changed && (loading || !has_document)
                    || private_changed && (loading || !private_mode && !has_document));
            if !restart {
                return;
            }
            let plan = state
                .settings
                .configured_lyrics_plan(private_mode, &track_id);
            self.begin_current(&mut state, &media_id, plan, false, selection_changed)
                .map(|prepared| {
                    (
                        prepared,
                        selection_changed
                            || karaoke_enabled
                            || (!settings_changed && !private_mode),
                    )
                })
        };
        if let Some((prepared, use_cache)) = restart {
            self.launch_current(prepared, use_cache);
        }
    }

    fn clear(&self) {
        self.next_request.fetch_add(1, Ordering::AcqRel);
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        cancel_current_work(&mut state);
        state.current = None;
        state.search = None;
        drop(state);
        self.publish(LyricsEvent::Current(CurrentLyrics::Cleared));
    }

    fn load(self: &Arc<Self>, media_id: CurrentMediaId) {
        let prepared = {
            let mut state = self
                .state
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            let Some(track_id) = state
                .current
                .as_ref()
                .filter(|current| current.context.media.id == media_id)
                .map(|current| current.key.media_uri.clone())
            else {
                return;
            };
            let plan = state
                .settings
                .automatic_lyrics_plan(state.private_mode, &track_id);
            self.begin_current(&mut state, &media_id, plan, true, false)
        };
        if let Some(prepared) = prepared {
            self.launch_current(prepared, true);
        }
    }

    fn begin_current(
        &self,
        state: &mut State,
        media_id: &CurrentMediaId,
        plan: LyricsPlan,
        automatic: bool,
        keep_visible: bool,
    ) -> Option<(u64, DocumentKey, LyricsContext, LyricsPlan, Arc<AtomicBool>)> {
        let current = state
            .current
            .as_ref()
            .filter(|current| &current.context.media.id == media_id)?;
        if automatic && (current.automatic_attempted || current.loading || current.bundle.is_some())
        {
            return None;
        }
        cancel_current_work(state);
        let selection = state.settings.clone();
        let current = state
            .current
            .as_mut()
            .expect("the checked current lyrics document is present");
        current.automatic_attempted = true;
        current.request = self.next_request.fetch_add(1, Ordering::AcqRel);
        current.loading = true;
        if keep_visible {
            let selected = current
                .bundle
                .as_ref()
                .and_then(|bundle| bundle.selected_document(&selection));
            let pronunciation = current.bundle.as_ref().and_then(|bundle| {
                selected.and_then(|document| bundle.pronunciation_for(document))
            });
            current.document = selected.map(|selected| {
                current
                    .document
                    .as_ref()
                    .filter(|visible| visible.as_ref() == selected)
                    .cloned()
                    .unwrap_or_else(|| Arc::new(selected.clone()))
            });
            current.pronunciation = pronunciation.map(|pronunciation| {
                current
                    .pronunciation
                    .as_ref()
                    .filter(|visible| visible.as_ref() == pronunciation)
                    .cloned()
                    .unwrap_or_else(|| Arc::new(pronunciation.clone()))
            });
        } else {
            current.document = None;
            current.pronunciation = None;
            current.bundle = None;
        }
        let request = current.request;
        let key = current.key.clone();
        let context = current.context.clone();
        let cancelled = Arc::new(AtomicBool::new(false));
        state.current_cancelled = Some(Arc::clone(&cancelled));
        Some((request, key, context, plan, cancelled))
    }

    fn launch_current(
        self: &Arc<Self>,
        prepared: (u64, DocumentKey, LyricsContext, LyricsPlan, Arc<AtomicBool>),
        use_cache: bool,
    ) {
        let event = {
            let state = self
                .state
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            state
                .current
                .as_ref()
                .filter(|current| current.request == prepared.0 && current.key == prepared.1)
                .map(current_event)
        };
        if let Some(event) = event {
            self.publish(event);
        }
        let context = prepared.2;
        let plan = prepared.3;
        let service = Arc::clone(self);
        let request = prepared.0;
        let key = prepared.1;
        let cancelled = prepared.4;
        let task_cancelled = Arc::clone(&cancelled);
        let task_key = key.clone();
        let task = self.runtime.spawn(async move {
            if !service.current_request_active(request, &task_key, &task_cancelled) {
                return;
            }
            let resolution = current_resolution(context, plan).await;
            service
                .resolve_current(request, task_key, resolution, task_cancelled, use_cache)
                .await;
        });
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if state
            .current
            .as_ref()
            .is_some_and(|current| current.request == request && current.key == key)
            && state
                .current_cancelled
                .as_ref()
                .is_some_and(|current| Arc::ptr_eq(current, &cancelled))
        {
            state.current_task = Some(task);
        } else {
            task.abort();
        }
    }

    async fn resolve_current(
        self: &Arc<Self>,
        request: u64,
        key: DocumentKey,
        mut resolution: CurrentResolution,
        cancelled: Arc<AtomicBool>,
        use_cache: bool,
    ) {
        let mut fallback = None;
        if let Some(input) = resolution.local.as_ref() {
            let input = input.clone();
            let document = tokio::task::spawn_blocking(move || local_sidecar_lyrics(&input))
                .await
                .ok()
                .flatten();
            if let Some(document) = document
                && self
                    .resolve_candidate(
                        request,
                        &key,
                        &resolution,
                        &cancelled,
                        &mut fallback,
                        document,
                    )
                    .await
            {
                return;
            }
        }
        if !self.current_request_active(request, &key, &cancelled) {
            return;
        }
        if !resolution.cue_track
            && let Some(input) = resolution.local.take()
        {
            let document =
                tokio::task::spawn_blocking(move || embedded_lyrics_from_audio(&input.audio_path))
                    .await
                    .ok()
                    .flatten();
            if let Some(document) = document
                && self
                    .resolve_candidate(
                        request,
                        &key,
                        &resolution,
                        &cancelled,
                        &mut fallback,
                        document,
                    )
                    .await
            {
                return;
            }
        }
        if !self.current_request_active(request, &key, &cancelled) {
            return;
        }

        for authority in acquisition_order(resolution.plan.prefers_server()) {
            if use_cache
                && self
                    .resolve_cached_candidate(
                        request,
                        &key,
                        &resolution,
                        &cancelled,
                        &mut fallback,
                        authority,
                    )
                    .await
            {
                return;
            }
            if !self.current_request_active(request, &key, &cancelled) {
                return;
            }
            let complete = match authority {
                LyricsAuthority::Source => {
                    self.resolve_source_candidate(
                        request,
                        &key,
                        &resolution,
                        &cancelled,
                        &mut fallback,
                    )
                    .await
                }
                LyricsAuthority::External => {
                    self.resolve_external_candidate(
                        request,
                        &key,
                        &resolution,
                        &cancelled,
                        &mut fallback,
                    )
                    .await
                }
            };
            if complete || !self.current_request_active(request, &key, &cancelled) {
                return;
            }
        }
        if let Some(document) = fallback {
            if self.current_request_active(request, &key, &cancelled) {
                if matches!(document.origin, LyricsOrigin::External(_)) {
                    self.cache_and_accept(
                        request,
                        &key,
                        &resolution.context.input_digest,
                        &resolution.plan,
                        document,
                    )
                    .await;
                } else {
                    self.accept_bundle(request, &key, Arc::new(document));
                }
            }
            return;
        }
        if self.current_request_active(request, &key, &cancelled) {
            self.finish_current(request, &key, None);
        }
    }

    async fn resolve_cached_candidate(
        self: &Arc<Self>,
        request: u64,
        key: &DocumentKey,
        resolution: &CurrentResolution,
        cancelled: &AtomicBool,
        fallback: &mut Option<LyricsBundle>,
        authority: LyricsAuthority,
    ) -> bool {
        let (role, language, script) = key.cache_key(&resolution.plan);
        let authority = match authority {
            LyricsAuthority::Source => "source",
            LyricsAuthority::External => "external",
        };
        let cached = self
            .database
            .lyrics_cache_for_role(
                &key.media_uri,
                &role,
                &language,
                &script,
                resolution.context.input_digest,
                authority,
                &ReadCancellation::new(),
            )
            .await
            .ok()
            .flatten();
        let Some(cached) = cached else {
            return false;
        };
        let candidate = cached_bundle(&cached)
            .and_then(lyrics_with_displayable_content)
            .filter(|document| {
                document.origin != LyricsOrigin::Local
                    && cached_lyrics_allowed(document, &resolution.plan, resolution.cue_track)
            });
        if let Some(document) = candidate {
            return self
                .resolve_candidate(request, key, resolution, cancelled, fallback, document)
                .await;
        }
        if self.current_request_active(request, key, cancelled) {
            let _ = self
                .database
                .remove_lyrics_cache(&key.media_uri, authority, &role, &language, &script)
                .await;
        }
        false
    }

    async fn resolve_source_candidate(
        self: &Arc<Self>,
        request: u64,
        key: &DocumentKey,
        resolution: &CurrentResolution,
        cancelled: &AtomicBool,
        fallback: &mut Option<LyricsBundle>,
    ) -> bool {
        let Some(source) = resolution.context.source().await else {
            return false;
        };
        match source.lyrics(&key.media_uri).await {
            Ok(Some(native)) => {
                self.resolve_candidate(
                    request,
                    key,
                    resolution,
                    cancelled,
                    fallback,
                    lyrics_from_native(native),
                )
                .await
            }
            Ok(None) => false,
            Err(error) => {
                debug!(%error, media_uri = %key.media_uri, "source lyrics request failed");
                false
            }
        }
    }

    async fn resolve_external_candidate(
        self: &Arc<Self>,
        request: u64,
        key: &DocumentKey,
        resolution: &CurrentResolution,
        cancelled: &Arc<AtomicBool>,
        fallback: &mut Option<LyricsBundle>,
    ) -> bool {
        if !resolution.plan.allows_external_fallback() {
            return false;
        }
        let lookup = resolution.lookup.clone();
        let providers = resolution.plan.external_providers().to_vec();
        let require_word_timing = resolution.plan.requires_word_timing();
        let prefer_translations = resolution.plan.prefers_translations();
        let preferred_translation_language =
            resolution.plan.preferred_translation_language().to_string();
        let lookup_cancelled = Arc::clone(cancelled);
        let document = run_external_lookup(Arc::clone(&self.search_lane), move || {
            external_best_lyrics(
                &lookup,
                &providers,
                require_word_timing,
                prefer_translations,
                &preferred_translation_language,
                &lookup_cancelled,
            )
        })
        .await
        .and_then(|result| match result {
            Ok(document) => document,
            Err(error) => {
                debug!(%error, "external lyrics request failed");
                None
            }
        });
        let Some(document) = document else {
            return false;
        };
        if acquisition_complete(&document, &resolution.plan) {
            if self.current_request_active(request, key, cancelled) {
                self.cache_and_accept(
                    request,
                    key,
                    &resolution.context.input_digest,
                    &resolution.plan,
                    document,
                )
                .await;
            }
            return true;
        }
        if prefer_fallback(fallback, document, &resolution.plan) {
            self.show_fallback(request, key, fallback.as_ref().expect("fallback"));
        }
        false
    }

    async fn resolve_candidate(
        self: &Arc<Self>,
        request: u64,
        key: &DocumentKey,
        resolution: &CurrentResolution,
        cancelled: &AtomicBool,
        fallback: &mut Option<LyricsBundle>,
        document: LyricsBundle,
    ) -> bool {
        if acquisition_complete(&document, &resolution.plan) {
            if self.current_request_active(request, key, cancelled) {
                self.cache_and_accept(
                    request,
                    key,
                    &resolution.context.input_digest,
                    &resolution.plan,
                    document,
                )
                .await;
            }
            return true;
        }
        if prefer_fallback(fallback, document, &resolution.plan) {
            self.show_fallback(request, key, fallback.as_ref().expect("fallback"));
        }
        false
    }

    async fn cache_and_accept(
        self: &Arc<Self>,
        request: u64,
        key: &DocumentKey,
        input: &[u8; 32],
        plan: &LyricsPlan,
        document: LyricsBundle,
    ) {
        if !self.matches_current(request, key) {
            return;
        }
        let write_target = matches!(document.origin, LyricsOrigin::External(_))
            .then(|| self.fetched_lyrics_write_target(request, key, &document))
            .flatten();
        if document.origin != LyricsOrigin::Local {
            match cache_write(key, plan, &document) {
                Ok((authority, role, language, script, payload, updated_at)) => {
                    if let Err(error) = self
                        .database
                        .write_lyrics_cache(
                            &key.media_uri,
                            &authority,
                            &role,
                            &language,
                            &script,
                            *input,
                            &payload,
                            updated_at,
                        )
                        .await
                    {
                        warn!(%error, "could not save lyrics cache");
                    }
                }
                Err(error) => warn!(%error, "could not encode lyrics cache"),
            }
        }
        self.accept_bundle(request, key, Arc::new(document));
        if let Some(target) = write_target {
            self.queue_lyrics_write(target);
        }
    }

    fn fetched_lyrics_write_target(
        &self,
        request: u64,
        key: &DocumentKey,
        lyrics: &LyricsBundle,
    ) -> Option<LyricsWriteTarget> {
        {
            let state = self
                .state
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            let Some(current) = state
                .current
                .as_ref()
                .filter(|current| current.request == request && &current.key == key)
            else {
                return None;
            };
            if !state.settings.save_lyrics_to_source || !state.settings.save_lyrics_automatically {
                return None;
            }
            let Some(document) = lyrics.selected_document(&state.settings) else {
                return None;
            };
            Some(LyricsWriteTarget {
                context: current.context.clone(),
                content: lyrics_to_lrc_text(document, 0),
                sidecar: state.settings.save_lyrics_as_sidecar,
            })
        }
    }

    async fn write_lyrics_to_source(&self, target: LyricsWriteTarget) {
        let Some(source) = target.context.source().await else {
            return;
        };
        if !source
            .lyrics_writable(
                &target.context.database,
                &target.context.media.media_uri,
                target.sidecar,
            )
            .await
        {
            return;
        }
        let Ok(_permit) = self.write_lane.acquire().await else {
            return;
        };
        if let Err(error) = source
            .write_lyrics(
                &target.context.database,
                &target.context.media.media_uri,
                &target.content,
                target.sidecar,
            )
            .await
        {
            warn!(%error, "could not save lyrics to source");
            self.publish(LyricsEvent::SourceSaveFailed {
                media_id: target.context.media.id.clone(),
                error,
            });
            return;
        }
    }

    fn queue_lyrics_write(self: &Arc<Self>, target: LyricsWriteTarget) {
        let service = Arc::clone(self);
        self.runtime.spawn(async move {
            service.write_lyrics_to_source(target).await;
        });
    }

    fn accept_bundle(&self, request: u64, key: &DocumentKey, bundle: Arc<LyricsBundle>) {
        self.finish_current(request, key, Some(bundle));
    }

    fn show_fallback(&self, request: u64, key: &DocumentKey, bundle: &LyricsBundle) {
        let event = {
            let mut state = self
                .state
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            let settings = state.settings.clone();
            let Some(current) = state
                .current
                .as_mut()
                .filter(|current| current.request == request && &current.key == key)
            else {
                return;
            };
            if current
                .bundle
                .as_deref()
                .is_some_and(|visible| visible == bundle)
            {
                return;
            }
            apply_bundle(current, &settings, Arc::new(bundle.clone()));
            current_event(current)
        };
        self.publish(event);
    }

    fn finish_current(&self, request: u64, key: &DocumentKey, bundle: Option<Arc<LyricsBundle>>) {
        let event = {
            let mut state = self
                .state
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            let settings = state.settings.clone();
            let Some(current) = state
                .current
                .as_mut()
                .filter(|current| current.request == request && &current.key == key)
            else {
                return;
            };
            current.loading = false;
            if let Some(bundle) = bundle {
                apply_bundle(current, &settings, bundle);
            } else if current.bundle.is_none() {
                current.pronunciation = None;
                current.bundle = None;
            }
            let event = current_event(current);
            state.current_cancelled = None;
            state.current_task = None;
            event
        };
        self.publish(event);
    }

    fn matches_current(&self, request: u64, key: &DocumentKey) -> bool {
        self.state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .current
            .as_ref()
            .is_some_and(|current| current.request == request && &current.key == key)
    }

    fn current_request_active(
        &self,
        request: u64,
        key: &DocumentKey,
        cancelled: &AtomicBool,
    ) -> bool {
        !cancelled.load(Ordering::Acquire) && self.matches_current(request, key)
    }

    fn search(self: &Arc<Self>, media_id: CurrentMediaId, query: LyricsQuery) {
        let query = LyricsQuery {
            artist_name: query.artist_name.trim().to_string(),
            track_name: query.track_name.trim().to_string(),
        };
        if query.artist_name.is_empty() && query.track_name.is_empty() {
            return;
        }
        let prepared = {
            let mut state = self
                .state
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            let Some(current) = state
                .current
                .as_ref()
                .filter(|current| current.context.media.id == media_id)
            else {
                return;
            };
            let key = current.key.clone();
            let event_media = current.context.media.id.clone();
            let allowed = state
                .settings
                .external_lyrics_network_allowed(state.private_mode);
            let providers = if allowed {
                state.settings.external_lyrics_providers.clone()
            } else {
                Vec::new()
            };
            let request = self.next_request.fetch_add(1, Ordering::AcqRel);
            state.search = Some(PendingSearch {
                request,
                key: key.clone(),
                query: query.clone(),
            });
            (request, key, event_media, providers)
        };
        if prepared.3.is_empty() {
            self.finish_search(prepared.0, &prepared.1, query, Ok(Vec::new()));
            return;
        }
        let service = Arc::clone(self);
        let _task = self.runtime.spawn(async move {
            let Ok(_permit) = Arc::clone(&service.search_lane).acquire_owned().await else {
                return;
            };
            if !service.matches_search(prepared.0, &prepared.1, &query) {
                return;
            }
            let artist = query.artist_name.clone();
            let track = query.track_name.clone();
            let result =
                tokio::task::spawn_blocking(move || search_lyrics(&prepared.3, &artist, &track))
                    .await
                    .map_err(|error| error.to_string())
                    .and_then(|result| result);
            service.finish_search(prepared.0, &prepared.1, query, result);
        });
    }

    fn matches_search(&self, request: u64, key: &DocumentKey, query: &LyricsQuery) -> bool {
        let state = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        state
            .current
            .as_ref()
            .is_some_and(|current| &current.key == key)
            && state.search.as_ref().is_some_and(|search| {
                search.request == request && &search.key == key && &search.query == query
            })
    }

    fn finish_search(
        &self,
        request: u64,
        key: &DocumentKey,
        query: LyricsQuery,
        result: Result<Vec<LyricsSearchResult>, String>,
    ) {
        let media_id = {
            let mut state = self
                .state
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            if !state.search.as_ref().is_some_and(|search| {
                search.request == request && &search.key == key && search.query == query
            }) {
                return;
            }
            state.search = None;
            let Some(current) = state.current.as_ref().filter(|current| &current.key == key) else {
                return;
            };
            current.context.media.id.clone()
        };
        self.publish(LyricsEvent::SearchFinished {
            media_id,
            query,
            result,
        });
    }

    fn preview(self: &Arc<Self>, media_id: CurrentMediaId, result: LyricsSearchResult) {
        let Some((request, key, input, plan)) = self.begin_external_document(&media_id, &result)
        else {
            return;
        };
        let service = Arc::clone(self);
        let _task = self.runtime.spawn(async move {
            let result =
                tokio::task::spawn_blocking(move || lyrics_from_search_result(&result)).await;
            match result {
                Ok(Ok(Some(document))) => {
                    service
                        .cache_and_accept(request, &key, &input, &plan, document)
                        .await;
                }
                Ok(Ok(None) | Err(_)) | Err(_) => service.finish_current(request, &key, None),
            }
        });
    }

    fn save_result(
        self: &Arc<Self>,
        media_id: CurrentMediaId,
        result: LyricsSearchResult,
        path: Option<PathBuf>,
    ) {
        let Some((request, key, input, plan)) = self.begin_external_document(&media_id, &result)
        else {
            return;
        };
        let service = Arc::clone(self);
        let save_to_source = path.is_none();
        let _task = self.runtime.spawn(async move {
            let save_plan = plan.clone();
            let saved = tokio::task::spawn_blocking(move || {
                let Some(bundle) = lyrics_from_search_result(&result)? else {
                    return Ok::<_, String>(None);
                };
                let selection = Settings {
                    prefer_translations: save_plan.prefers_translations(),
                    preferred_translation_language: save_plan
                        .preferred_translation_language()
                        .to_string(),
                    ..Settings::default()
                };
                let Some(document) = bundle.selected_document(&selection) else {
                    return Ok(None);
                };
                let source_content = save_to_source.then(|| lyrics_to_lrc_text(document, 0));
                let path = path
                    .map(|path| save_current_lyrics(document, 0, path))
                    .transpose()?;
                Ok(Some((path, bundle, source_content)))
            })
            .await;
            match saved {
                Ok(Ok(Some((path, document, source_content)))) => {
                    let document = Arc::new(document);
                    if let Some(content) = source_content {
                        if service.matches_current(request, &key) {
                            service.accept_bundle(request, &key, Arc::clone(&document));
                        }
                        service.update_lyrics_text(media_id, content);
                        return;
                    }
                    if let Ok((authority, role, language, script, payload, updated_at)) =
                        cache_write(&key, &plan, &document)
                    {
                        let _ = service
                            .database
                            .write_lyrics_cache(
                                &key.media_uri,
                                &authority,
                                &role,
                                &language,
                                &script,
                                input,
                                &payload,
                                updated_at,
                            )
                            .await;
                    }
                    let write_target =
                        service.fetched_lyrics_write_target(request, &key, &document);
                    let accepted = service.matches_current(request, &key);
                    if accepted {
                        service.accept_bundle(request, &key, Arc::clone(&document));
                    }
                    if let Some(target) = write_target {
                        service.queue_lyrics_write(target);
                    }
                    if let Some(path) = path {
                        service.publish(LyricsEvent::Saved { media_id, path });
                    }
                }
                Ok(Ok(None) | Err(_)) | Err(_) => service.finish_current(request, &key, None),
            }
        });
    }

    fn begin_external_document(
        &self,
        media_id: &CurrentMediaId,
        result: &LyricsSearchResult,
    ) -> Option<(u64, DocumentKey, [u8; 32], LyricsPlan)> {
        let prepared = {
            let mut state = self
                .state
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            if !state
                .settings
                .external_lyrics_network_allowed(state.private_mode)
                || !state
                    .settings
                    .external_lyrics_providers
                    .contains(&result.provider)
            {
                return None;
            }
            cancel_current_work(&mut state);
            let track_id = state
                .current
                .as_ref()
                .filter(|current| &current.context.media.id == media_id)?
                .key
                .media_uri
                .clone();
            let plan = state
                .settings
                .configured_lyrics_plan(state.private_mode, &track_id);
            let current = state
                .current
                .as_mut()
                .filter(|current| &current.context.media.id == media_id)?;
            current.request = self.next_request.fetch_add(1, Ordering::AcqRel);
            current.loading = true;
            current.document = None;
            current.pronunciation = None;
            current.bundle = None;
            (
                current.request,
                current.key.clone(),
                current.context.input_digest,
                plan,
                current.context.media.id.clone(),
            )
        };
        self.publish(LyricsEvent::Current(CurrentLyrics::Loading {
            media_id: prepared.4,
        }));
        Some((prepared.0, prepared.1, prepared.2, prepared.3))
    }

    fn save_current(
        self: &Arc<Self>,
        media_id: CurrentMediaId,
        offset_millis: i64,
        path: Option<PathBuf>,
    ) {
        let document = {
            let state = self
                .state
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            state
                .current
                .as_ref()
                .filter(|current| current.context.media.id == media_id)
                .and_then(|current| current.document.clone())
        };
        let Some(document) = document else {
            return;
        };
        let Some(path) = path else {
            self.update_lyrics_text(media_id, lyrics_to_lrc_text(&document, offset_millis));
            return;
        };
        let service = Arc::clone(self);
        let _task = self.runtime.spawn(async move {
            let saved = tokio::task::spawn_blocking(move || {
                save_current_lyrics(&document, offset_millis, path)
            })
            .await;
            if let Ok(Ok(path)) = saved {
                service.publish(LyricsEvent::Saved { media_id, path });
            }
        });
    }

    fn update_lyrics_text(self: &Arc<Self>, media_id: CurrentMediaId, text: String) {
        let Some(bundle) = lyrics_from_edited_text(&text) else {
            return;
        };
        let Some(document) = bundle.documents().first() else {
            return;
        };
        let prepared = {
            let mut state = self
                .state
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            let Some(current) = state
                .current
                .as_ref()
                .filter(|current| current.context.media.id == media_id)
            else {
                return;
            };
            let key = current.key.clone();
            let context = current.context.clone();
            let plan = state
                .settings
                .configured_lyrics_plan(state.private_mode, &key.media_uri);
            let sidecar = state.settings.save_lyrics_as_sidecar;
            cancel_current_work(&mut state);
            let request = self.next_request.fetch_add(1, Ordering::AcqRel);
            if let Some(current) = state.current.as_mut() {
                current.request = request;
                current.loading = false;
            }
            (request, plan, context, key, sidecar)
        };
        let service = Arc::clone(self);
        let (request, plan, context, key, sidecar) = prepared;
        let content = lyrics_to_lrc_text(document, 0);
        let _task = self.runtime.spawn(async move {
            let Some(source) = context.source().await else {
                return;
            };
            let Ok(_permit) = service.write_lane.acquire().await else {
                return;
            };
            match source
                .write_lyrics(&context.database, &key.media_uri, &content, sidecar)
                .await
            {
                Ok(()) => match source.refresh_written_lyrics().await {
                    Ok(()) => {
                        service
                            .cache_and_accept(request, &key, &context.input_digest, &plan, bundle)
                            .await;
                    }
                    Err(error) => {
                        warn!(%error, "could not refresh edited lyrics source");
                        service.publish(LyricsEvent::SourceSaveFailed { media_id, error });
                    }
                },
                Err(error) => {
                    warn!(%error, "could not save edited lyrics");
                    service.publish(LyricsEvent::SourceSaveFailed { media_id, error });
                }
            }
        });
    }

    fn clear_fetched(self: &Arc<Self>, media_id: CurrentMediaId) {
        let key = {
            let state = self
                .state
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            state
                .current
                .as_ref()
                .filter(|current| current.context.media.id == media_id)
                .map(|current| current.key.clone())
        };
        let Some(key) = key else {
            return;
        };
        let database = self.database.clone();
        let _task = self.runtime.spawn(async move {
            if let Err(error) = database
                .remove_track_lyrics_by_authority(&key.media_uri, "external")
                .await
            {
                warn!(%error, "could not clear fetched lyrics cache");
            }
        });
    }

    fn publish(&self, event: LyricsEvent) {
        let _ = self.events.try_send(event);
    }
}

impl LyricsHandle {
    pub fn japanese_dictionary(&self, retry: bool) -> crate::JapaneseDictionaryStatus {
        use crate::JapaneseDictionaryStatus;

        let mut status = self
            .service
            .dictionary_status
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        if matches!(*status, JapaneseDictionaryStatus::Idle)
            || retry && matches!(*status, JapaneseDictionaryStatus::Failed)
        {
            *status = JapaneseDictionaryStatus::Loading;
            self.service
                .publish(LyricsEvent::JapaneseDictionaryChanged(status.clone()));
            let service = Arc::clone(&self.service);
            self.service.runtime.spawn_blocking(move || {
                let result = crate::dictionary::prepare_dictionary(&service.dictionary_directory);
                let status = match result {
                    Ok(path) => JapaneseDictionaryStatus::Ready(path),
                    Err(error) => {
                        warn!(%error, "could not prepare Japanese dictionary");
                        JapaneseDictionaryStatus::Failed
                    }
                };
                *service
                    .dictionary_status
                    .lock()
                    .unwrap_or_else(|e| e.into_inner()) = status.clone();
                service.publish(LyricsEvent::JapaneseDictionaryChanged(status));
            });
        }
        status.clone()
    }

    pub fn load(&self, media_id: CurrentMediaId) {
        self.service.load(media_id);
    }

    pub fn search(&self, media_id: CurrentMediaId, query: LyricsQuery) {
        self.service.search(media_id, query);
    }

    pub fn preview(&self, media_id: CurrentMediaId, result: LyricsSearchResult) {
        self.service.preview(media_id, result);
    }

    pub fn save_result(&self, media_id: CurrentMediaId, result: LyricsSearchResult, path: PathBuf) {
        self.service.save_result(media_id, result, Some(path));
    }

    pub fn save_result_to_source(&self, media_id: CurrentMediaId, result: LyricsSearchResult) {
        self.service.save_result(media_id, result, None);
    }

    pub fn save_current(&self, media_id: CurrentMediaId, offset_millis: i64, path: PathBuf) {
        self.service
            .save_current(media_id, offset_millis, Some(path));
    }

    pub fn save_current_to_source(&self, media_id: CurrentMediaId, offset_millis: i64) {
        self.service.save_current(media_id, offset_millis, None);
    }

    pub fn update_lyrics_text(&self, media_id: CurrentMediaId, text: String) {
        self.service.update_lyrics_text(media_id, text);
    }

    pub fn current_writable(&self, media_id: &CurrentMediaId) -> bool {
        self.service
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .current
            .as_ref()
            .is_some_and(|current| &current.context.media.id == media_id && current.writable)
    }

    pub fn refresh_write_access(&self) {
        self.service.refresh_write_access();
    }

    pub fn clear_fetched(&self, media_id: CurrentMediaId) {
        self.service.clear_fetched(media_id);
    }
}

fn current_event(current: &CurrentDocument) -> LyricsEvent {
    if current.loading && current.bundle.is_none() {
        LyricsEvent::Current(CurrentLyrics::Loading {
            media_id: current.context.media.id.clone(),
        })
    } else {
        LyricsEvent::Current(CurrentLyrics::Ready {
            media_id: current.context.media.id.clone(),
            content: current.bundle.as_ref().and_then(|bundle| {
                if bundle.is_instrumental() {
                    Some(CurrentLyricsContent::Instrumental)
                } else {
                    current
                        .document
                        .clone()
                        .map(|document| CurrentLyricsContent::Document {
                            document,
                            pronunciation: current.pronunciation.clone(),
                        })
                }
            }),
            origin: current.bundle.as_ref().map(|bundle| bundle.origin),
        })
    }
}

fn apply_bundle(current: &mut CurrentDocument, settings: &Settings, bundle: Arc<LyricsBundle>) {
    let selected = bundle.selected_document(settings);
    current.pronunciation = selected
        .and_then(|document| bundle.pronunciation_for(document))
        .cloned()
        .map(Arc::new);
    current.document = selected.cloned().map(Arc::new);
    current.bundle = Some(bundle);
}

fn selection_settings(plan: &LyricsPlan) -> Settings {
    Settings {
        prefer_translations: plan.prefers_translations(),
        preferred_translation_language: plan.preferred_translation_language().to_string(),
        ..Settings::default()
    }
}

fn bundle_satisfies_plan(bundle: &LyricsBundle, plan: &LyricsPlan) -> bool {
    if bundle.is_instrumental() {
        true
    } else if plan.prefers_translations() {
        bundle.has_preferred_translation(&selection_settings(plan))
    } else {
        bundle.has_original()
    }
}

fn acquisition_complete(bundle: &LyricsBundle, plan: &LyricsPlan) -> bool {
    bundle.is_instrumental()
        || bundle_satisfies_plan(bundle, plan)
            && (!plan.requires_word_timing()
                || bundle
                    .selected_document(&selection_settings(plan))
                    .is_some_and(LyricsDocument::has_word_timing))
}

fn prefer_fallback(
    fallback: &mut Option<LyricsBundle>,
    candidate: LyricsBundle,
    plan: &LyricsPlan,
) -> bool {
    let replace = fallback.as_ref().is_none_or(|current| {
        !bundle_satisfies_plan(current, plan) && bundle_satisfies_plan(&candidate, plan)
    });
    if replace {
        *fallback = Some(candidate);
    }
    replace
}

fn cache_key_for_bundle(
    key: &DocumentKey,
    plan: &LyricsPlan,
    bundle: &LyricsBundle,
) -> (String, String, String) {
    if plan.prefers_translations() && bundle.has_preferred_translation(&selection_settings(plan)) {
        key.cache_key(plan)
    } else {
        key.cache_key_for(LyricsRole::Original, "")
    }
}

fn lyrics_selection_changed(previous: &Settings, current: &Settings) -> bool {
    previous.prefer_translations != current.prefer_translations
        || previous.preferred_translation_language != current.preferred_translation_language
}

fn acquisition_order(prefer_server: bool) -> [LyricsAuthority; 2] {
    if prefer_server {
        [LyricsAuthority::Source, LyricsAuthority::External]
    } else {
        [LyricsAuthority::External, LyricsAuthority::Source]
    }
}

fn lyrics_acquisition_changed(previous: &Settings, current: &Settings, track_id: &str) -> bool {
    let previous_external =
        previous.external_lyrics_enabled && !previous.auto_lyrics_suppressed(track_id);
    let current_external =
        current.external_lyrics_enabled && !current.auto_lyrics_suppressed(track_id);
    previous_external != current_external
        || (previous_external || current_external)
            && (previous.prefer_server_lyrics != current.prefer_server_lyrics
                || previous.external_lyrics_providers != current.external_lyrics_providers)
}

fn cancel_current_work(state: &mut State) {
    if let Some(cancelled) = state.current_cancelled.take() {
        cancelled.store(true, Ordering::Release);
    }
    if let Some(task) = state.current_task.take() {
        task.abort();
    }
}

async fn run_external_lookup(
    lane: Arc<Semaphore>,
    lookup: impl FnOnce() -> Result<Option<LyricsBundle>, String> + Send + 'static,
) -> Option<Result<Option<LyricsBundle>, String>> {
    let permit = lane.acquire_owned().await.ok()?;
    tokio::task::spawn_blocking(move || {
        let _permit = permit;
        lookup()
    })
    .await
    .ok()
}

async fn current_resolution(context: LyricsContext, plan: LyricsPlan) -> CurrentResolution {
    let cue_track = library::cue_media_parts(&context.media.media_uri).is_some();
    let access = context
        .database
        .playback_access_uri(&context.media.media_uri)
        .await
        .ok()
        .flatten();
    let local = access
        .as_deref()
        .and_then(local_audio_path)
        .or_else(|| local_audio_path(&context.media.media_uri))
        .map(|path| LocalLyricsInput {
            audio_path: path,
            title: context.media.title.clone(),
            cue_track,
        });
    CurrentResolution {
        lookup: LyricsLookup::from_search(
            &context.media.artist,
            &context.media.title,
            u32::try_from(context.media.duration_millis.max(0) / 1_000).unwrap_or(u32::MAX),
        ),
        local,
        cue_track,
        plan,
        context,
    }
}

fn local_audio_path(media_uri: &str) -> Option<PathBuf> {
    match library::cue_media_parts(media_uri) {
        Some((_, uri, _, _)) => library::file_media_path(&uri),
        None => library::file_media_path(media_uri),
    }
}

fn cached_bundle(cached: &library::LyricsCacheRow) -> Option<LyricsBundle> {
    decode_cached_bundle(&cached.lyrics)
}

fn decode_cached_bundle(payload: &str) -> Option<LyricsBundle> {
    let cached = serde_json::from_str::<CachedBundle>(payload).ok()?;
    (cached.version == LYRICS_CACHE_PAYLOAD_VERSION).then_some(cached.bundle)
}

fn cache_write(
    key: &DocumentKey,
    plan: &LyricsPlan,
    document: &LyricsBundle,
) -> Result<(String, String, String, String, String, i64), serde_json::Error> {
    let authority = match document.origin {
        LyricsOrigin::Local | LyricsOrigin::Native => "source",
        LyricsOrigin::External(_) => "external",
    };
    let (role, language, script) = cache_key_for_bundle(key, plan, document);
    Ok((
        authority.to_string(),
        role,
        language,
        script,
        serde_json::to_string(&CachedBundle {
            version: LYRICS_CACHE_PAYLOAD_VERSION,
            bundle: document.clone(),
        })?,
        unix_seconds(),
    ))
}

fn unix_seconds() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs().min(i64::MAX as u64) as i64)
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{LyricsCue, LyricsCueLine, LyricsLine};

    #[tokio::test]
    async fn current_lyrics_reads_local_cue_mapped_and_downloaded_files_by_logical_identity() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let database = Database::open(directory.path().join("library.db"))
            .await
            .expect("database");
        let audio = directory.path().join("album.flac");
        std::fs::write(&audio, []).expect("audio file");
        std::fs::write(audio.with_extension("lrc"), "[00:01.00]Whole file lyrics")
            .expect("sidecar");
        std::fs::write(
            directory.path().join("Segment.lrc"),
            "[00:01.00]Segment lyrics",
        )
        .expect("CUE sidecar");
        let file_uri = reqwest::Url::from_file_path(&audio)
            .expect("file URI")
            .to_string();
        let cue_uri = library::cue_media_uri("segment", &file_uri, 1_000, 3_000);
        let mapped_uri = library::source_entity_uri(&SourceId::new("mapped"), "track", "mapped");
        let downloaded_uri =
            library::source_entity_uri(&SourceId::new("mapped"), "track", "downloaded");
        let original = directory.path().join("original.flac");
        let mut locators = String::from("{\"version\":1}\n");
        for (media_uri, origin, path) in [
            (&mapped_uri, "mapping", &audio),
            (&downloaded_uri, "mapping", &original),
            (&downloaded_uri, "download", &audio),
        ] {
            locators.push_str(
                &serde_json::to_string(&library::LocalLocatorWrite {
                    source_id: None,
                    media_uri: media_uri.clone(),
                    origin: origin.to_string(),
                    path: path.to_string_lossy().into_owned(),
                    root: directory.path().to_string_lossy().into_owned(),
                    relative_path: path
                        .file_name()
                        .expect("file name")
                        .to_string_lossy()
                        .into_owned(),
                    access_uri: reqwest::Url::from_file_path(path)
                        .expect("access URI")
                        .to_string(),
                })
                .expect("locator"),
            );
            locators.push('\n');
        }
        database
            .import_local_locators_jsonl(std::io::Cursor::new(locators))
            .await
            .expect("local access");
        let (events, received) = async_channel::unbounded();
        let service = LyricsService::new(
            database.clone(),
            tokio::runtime::Handle::current(),
            Settings {
                external_lyrics_enabled: false,
                ..Settings::default()
            },
            false,
            events,
            directory.path().join("japanese-readings"),
        );
        for (index, (media_uri, title, expected)) in [
            (&file_uri, "Album", "Whole file lyrics"),
            (&cue_uri, "Segment", "Segment lyrics"),
            (&mapped_uri, "Mapped", "Whole file lyrics"),
            (&downloaded_uri, "Downloaded", "Whole file lyrics"),
        ]
        .into_iter()
        .enumerate()
        {
            let occurrence = library::OccurrenceId::new(index.to_string());
            let media_id = CurrentMediaId {
                run: None,
                occurrence: occurrence.clone(),
            };
            service.set_current(Some(LyricsContext {
                media: Arc::new(CurrentMedia {
                    id: media_id.clone(),
                    occurrence: Arc::new(library::QueueOccurrence {
                        occurrence,
                        item: library::QueueItem::direct(
                            media_uri, title, "Artist", "Album", 3_000,
                        ),
                        canonical_position: 0,
                        provenance: playback::Provenance::Manual,
                    }),
                }),
                input_digest: [0; 32],
                source: Arc::new(|_| None),
                database: database.clone(),
            }));
            service.handle().load(media_id.clone());
            let document = tokio::time::timeout(std::time::Duration::from_secs(5), async {
                loop {
                    if let LyricsEvent::Current(CurrentLyrics::Ready {
                        media_id: actual,
                        content: Some(CurrentLyricsContent::Document { document, .. }),
                        origin: Some(LyricsOrigin::Local),
                    }) = received.recv().await.expect("lyrics event")
                        && actual == media_id
                    {
                        break document;
                    }
                }
            })
            .await
            .expect("current lyrics");
            assert_eq!(document.lines[0].text, expected);
        }
        assert_eq!(
            database
                .original_file_path(&downloaded_uri)
                .await
                .expect("original path"),
            Some(original.to_string_lossy().into_owned())
        );
    }

    #[test]
    fn server_preference_owns_source_and_external_order() {
        assert_eq!(
            acquisition_order(true),
            [LyricsAuthority::Source, LyricsAuthority::External],
        );
        assert_eq!(
            acquisition_order(false),
            [LyricsAuthority::External, LyricsAuthority::Source],
        );
    }

    #[test]
    fn karaoke_requires_word_timing_for_every_lyrics_origin() {
        let plan = Settings {
            karaoke_mode: true,
            ..Settings::default()
        }
        .automatic_lyrics_plan(false, "track");
        for origin in [
            LyricsOrigin::Local,
            LyricsOrigin::Native,
            LyricsOrigin::External(crate::ExternalLyricsProvider::Lrclib),
        ] {
            assert!(!acquisition_complete(&bundle(origin, false), &plan));
            assert!(acquisition_complete(&bundle(origin, true), &plan));
        }
    }

    #[test]
    fn ordinary_lyrics_complete_acquisition_while_karaoke_is_off() {
        let plan = Settings::default().automatic_lyrics_plan(false, "track");

        assert!(acquisition_complete(
            &bundle(LyricsOrigin::Local, false),
            &plan,
        ));
    }

    #[test]
    fn regular_external_lyrics_do_not_replace_an_existing_local_fallback() {
        let plan = Settings::default().automatic_lyrics_plan(false, "track");
        let mut fallback = Some(bundle(LyricsOrigin::Local, false));

        assert!(!prefer_fallback(
            &mut fallback,
            bundle(
                LyricsOrigin::External(crate::ExternalLyricsProvider::Lrclib),
                false,
            ),
            &plan,
        ));
        assert_eq!(fallback.expect("fallback").origin, LyricsOrigin::Local);
    }

    fn bundle(origin: LyricsOrigin, word_timed: bool) -> LyricsBundle {
        let text = "A line".to_string();
        let cue_lines = word_timed
            .then(|| {
                vec![LyricsCueLine {
                    text: text.clone(),
                    start_millis: Some(1_000),
                    end_millis: Some(2_000),
                    agent_id: None,
                    cues: vec![LyricsCue {
                        text: text.clone(),
                        start_millis: 1_000,
                        end_millis: Some(2_000),
                        byte_start: 0,
                        byte_end_exclusive: text.len(),
                    }],
                }]
            })
            .unwrap_or_default();
        LyricsBundle::from_documents(
            origin,
            vec![LyricsDocument {
                role: LyricsRole::Original,
                language: None,
                offset_millis: 0,
                lines: vec![LyricsLine {
                    text,
                    start_millis: Some(1_000),
                    end_millis: Some(2_000),
                    cue_lines,
                }],
                agents: Vec::new(),
            }],
        )
    }
}
