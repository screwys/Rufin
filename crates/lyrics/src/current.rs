//! One current lyrics document and its user operations.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use async_channel::Sender;
use library::{Database, ReadCancellation, SourceKey, TrackKey};
use playback::{CurrentMedia, CurrentMediaId, PlaybackMedia, SourceSessionEpoch};
use serde::{Deserialize, Serialize};
use sources::{Source, SourceInputIdentity};
use tokio::sync::Semaphore;
use tokio::task::JoinHandle;
use tracing::{debug, warn};

use crate::lyrics::{
    LyricsPlan, cached_lyrics_allowed, external_best_lyrics, local_sidecar_lyrics,
    lyrics_from_native, lyrics_with_displayable_content,
};
use crate::{
    CurrentLyrics, CurrentLyricsContent, LocalLyricsInput, LyricsBundle, LyricsDocument,
    LyricsEvent, LyricsOrigin, LyricsQuery, LyricsRole, LyricsSearchResult, Settings,
    lyrics_from_search_result, save_current_lyrics, search_lyrics,
};

const LYRICS_CACHE_PAYLOAD_VERSION: u32 = 3;

#[derive(Clone)]
pub struct LyricsContext {
    pub media: Arc<CurrentMedia>,
    pub input: SourceInputIdentity,
    pub source: Option<Arc<Source>>,
    pub database: Database,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct DocumentKey {
    source_key: SourceKey,
    source_session_epoch: SourceSessionEpoch,
    track_key: Option<TrackKey>,
    track_object_id: String,
}

impl DocumentKey {
    fn for_context(context: &LyricsContext) -> Self {
        Self {
            source_key: context.media.id.source_key,
            source_session_epoch: context.media.id.source_session_epoch,
            track_key: context.media.track.track_key,
            track_object_id: context.media.track.track_object_id.clone(),
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
    input: SourceInputIdentity,
    source: Option<Arc<Source>>,
    track: PlaybackMedia,
    local: Option<LocalLyricsInput>,
    cue_track: bool,
    plan: LyricsPlan,
}

#[derive(Deserialize, Serialize)]
struct CachedBundle {
    version: u32,
    bundle: LyricsBundle,
}

pub struct LyricsService {
    database: Database,
    runtime: tokio::runtime::Handle,
    events: Sender<LyricsEvent>,
    state: Mutex<State>,
    next_request: AtomicU64,
    search_lane: Arc<Semaphore>,
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
    ) -> Arc<Self> {
        Arc::new(Self {
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
        let event = {
            let mut state = self
                .state
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            if let Some(current) = state.current.as_mut().filter(|current| current.key == key) {
                let media_changed = current.context.media.id != context.media.id;
                current.context = context;
                media_changed.then(|| current_event(current))
            } else {
                cancel_current_work(&mut state);
                let request = self.next_request.fetch_add(1, Ordering::AcqRel);
                let current = CurrentDocument {
                    context,
                    key,
                    document: None,
                    pronunciation: None,
                    bundle: None,
                    request,
                    loading: false,
                    automatic_attempted: false,
                };
                let event = Some(current_event(&current));
                state.current = Some(current);
                state.search = None;
                event
            }
        };
        if let Some(event) = event {
            self.publish(event);
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
                    current.key.track_object_id.clone(),
                    current.automatic_attempted || current.loading || current.bundle.is_some(),
                    current.loading,
                    current.bundle.is_some(),
                )
            });
            let selection_changed = lyrics_selection_changed(&state.settings, &settings);
            let acquisition_changed = current.as_ref().is_some_and(|(_, track_id, ..)| {
                lyrics_acquisition_changed(&state.settings, &settings, track_id)
            });
            let settings_changed = selection_changed || acquisition_changed;
            let private_changed = state.private_mode != private_mode;
            state.settings = settings;
            state.private_mode = private_mode;
            if !settings_changed && !private_changed {
                return;
            }
            state.search = None;
            let Some((media_id, track_id, attempted, loading, has_document)) = current else {
                return;
            };
            let restart = attempted
                && (selection_changed
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
                        selection_changed || (!settings_changed && !private_mode),
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
                .map(|current| current.context.media.track.track_object_id.clone())
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
        let resolution = current_resolution(prepared.2, prepared.3);
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
        let settings = Settings {
            prefer_translations: resolution.plan.prefers_translations(),
            preferred_translation_language: resolution
                .plan
                .preferred_translation_language()
                .to_string(),
            ..Settings::default()
        };
        let mut fallback = None;
        if let Some(input) = resolution.local.take() {
            let document = tokio::task::spawn_blocking(move || local_sidecar_lyrics(&input))
                .await
                .ok()
                .flatten();
            if let Some(document) = document {
                if document.is_instrumental()
                    || (!resolution.plan.prefers_translations() && document.has_original())
                    || document.has_preferred_translation(&settings)
                {
                    if self.current_request_active(request, &key, &cancelled) {
                        self.cache_and_accept(
                            request,
                            &key,
                            &resolution.input,
                            &resolution.plan,
                            document,
                        )
                        .await;
                    }
                    return;
                } else {
                    fallback = Some(document);
                }
            }
        }
        if !self.current_request_active(request, &key, &cancelled) {
            return;
        }

        if use_cache {
            let cached = if let Some(track_key) = key.track_key {
                let (role, language, script) = key.cache_key(&resolution.plan);
                let cancellation = ReadCancellation::new();
                self.database
                    .lyrics_cache_for_role(
                        key.source_key,
                        track_key,
                        &role,
                        &language,
                        &script,
                        resolution.input.digest,
                        &cancellation,
                    )
                    .await
                    .ok()
                    .flatten()
            } else {
                None
            };
            if let Some(cached) = cached {
                let authority = cached.authority.clone();
                if let Some(document) = cached_bundle(&cached)
                    .and_then(lyrics_with_displayable_content)
                    .filter(|document| {
                        cached_lyrics_allowed(document, &resolution.plan, resolution.cue_track)
                            && bundle_satisfies_plan(document, &resolution.plan)
                    })
                {
                    self.accept_bundle(request, &key, Arc::new(document));
                    return;
                }
                if !self.current_request_active(request, &key, &cancelled) {
                    return;
                }
                if let Some(track_key) = key.track_key {
                    let (role, language, script) = key.cache_key(&resolution.plan);
                    let _ = self
                        .database
                        .remove_lyrics_cache(
                            key.source_key,
                            track_key,
                            &authority,
                            &role,
                            &language,
                            &script,
                        )
                        .await;
                }
            }
        }
        if !self.current_request_active(request, &key, &cancelled) {
            return;
        }

        if let Some(source) = resolution.source.take() {
            match source
                .lyrics(&key.track_object_id, resolution.plan.native_search())
                .await
            {
                Ok(Some(native)) => {
                    let document = lyrics_from_native(native);
                    if document.is_instrumental()
                        || (!resolution.plan.prefers_translations() && document.has_original())
                        || document.has_preferred_translation(&settings)
                    {
                        if self.current_request_active(request, &key, &cancelled) {
                            self.cache_and_accept(
                                request,
                                &key,
                                &resolution.input,
                                &resolution.plan,
                                document,
                            )
                            .await;
                        }
                        return;
                    }
                    if fallback.is_none() && document.has_original() {
                        fallback = Some(document);
                    }
                }
                Ok(None) => {}
                Err(error) => {
                    debug!(%error, track_id = %key.track_object_id, "native lyrics request failed");
                }
            }
        }
        if !self.current_request_active(request, &key, &cancelled) {
            return;
        }

        if resolution.plan.allows_external_fallback() {
            let track = resolution.track;
            let providers = resolution.plan.external_providers().to_vec();
            let prefer_translations = resolution.plan.prefers_translations();
            let preferred_translation_language =
                resolution.plan.preferred_translation_language().to_string();
            let lookup_cancelled = Arc::clone(&cancelled);
            let document = run_external_lookup(Arc::clone(&self.search_lane), move || {
                external_best_lyrics(
                    &track,
                    &providers,
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
            if let Some(document) = document {
                let selected = document.is_instrumental()
                    || (!resolution.plan.prefers_translations() && document.has_original())
                    || document.has_preferred_translation(&settings);
                if selected {
                    if self.current_request_active(request, &key, &cancelled) {
                        self.cache_and_accept(
                            request,
                            &key,
                            &resolution.input,
                            &resolution.plan,
                            document,
                        )
                        .await;
                    }
                    return;
                }
                if fallback.is_none() && document.has_original() {
                    fallback = Some(document);
                }
            }
        }
        if let Some(document) = fallback {
            if self.current_request_active(request, &key, &cancelled) {
                self.accept_bundle(request, &key, Arc::new(document));
            }
            return;
        }
        if self.current_request_active(request, &key, &cancelled) {
            self.finish_current(request, &key, None);
        }
    }

    async fn cache_and_accept(
        &self,
        request: u64,
        key: &DocumentKey,
        input: &SourceInputIdentity,
        plan: &LyricsPlan,
        document: LyricsBundle,
    ) {
        if !self.matches_current(request, key) {
            return;
        }
        match cache_write(key, plan, &document) {
            Ok((authority, role, language, script, payload, updated_at)) => {
                if let Some(track_key) = key.track_key
                    && let Err(error) = self
                        .database
                        .write_lyrics_cache(
                            key.source_key,
                            track_key,
                            &authority,
                            &role,
                            &language,
                            &script,
                            input.digest,
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
        self.accept_bundle(request, key, Arc::new(document));
    }

    fn accept_bundle(&self, request: u64, key: &DocumentKey, bundle: Arc<LyricsBundle>) {
        self.finish_current(request, key, Some(bundle));
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
                let selected = bundle.selected_document(&settings);
                current.pronunciation = selected
                    .and_then(|document| bundle.pronunciation_for(document))
                    .cloned()
                    .map(Arc::new);
                current.document = selected.cloned().map(Arc::new);
                current.bundle = Some(bundle);
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
        path: PathBuf,
    ) {
        let Some((request, key, input, plan)) = self.begin_external_document(&media_id, &result)
        else {
            return;
        };
        let service = Arc::clone(self);
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
                let path = save_current_lyrics(document, 0, path)?;
                Ok(Some((path, bundle)))
            })
            .await;
            match saved {
                Ok(Ok(Some((path, document)))) => {
                    let document = Arc::new(document);
                    if let (
                        Some(track_key),
                        Ok((authority, role, language, script, payload, updated_at)),
                    ) = (key.track_key, cache_write(&key, &plan, &document))
                    {
                        let _ = service
                            .database
                            .write_lyrics_cache(
                                key.source_key,
                                track_key,
                                &authority,
                                &role,
                                &language,
                                &script,
                                input.digest,
                                &payload,
                                updated_at,
                            )
                            .await;
                    }
                    let accepted = service.matches_current(request, &key);
                    if accepted {
                        service.accept_bundle(request, &key, Arc::clone(&document));
                    }
                    service.publish(LyricsEvent::Saved { media_id, path });
                }
                Ok(Ok(None) | Err(_)) | Err(_) => service.finish_current(request, &key, None),
            }
        });
    }

    fn begin_external_document(
        &self,
        media_id: &CurrentMediaId,
        result: &LyricsSearchResult,
    ) -> Option<(u64, DocumentKey, SourceInputIdentity, LyricsPlan)> {
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
                .track_object_id
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
                current.context.input.clone(),
                plan,
                current.context.media.id.clone(),
            )
        };
        self.publish(LyricsEvent::Current(CurrentLyrics::Loading {
            media_id: prepared.4,
        }));
        Some((prepared.0, prepared.1, prepared.2, prepared.3))
    }

    fn save_current(self: &Arc<Self>, media_id: CurrentMediaId, offset_millis: i64, path: PathBuf) {
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
            let Some(track_key) = key.track_key else {
                return;
            };
            if let Err(error) = database
                .remove_track_lyrics_by_authority(key.source_key, track_key, "external")
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
        self.service.save_result(media_id, result, path);
    }

    pub fn save_current(&self, media_id: CurrentMediaId, offset_millis: i64, path: PathBuf) {
        self.service.save_current(media_id, offset_millis, path);
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

fn current_resolution(context: LyricsContext, plan: LyricsPlan) -> CurrentResolution {
    let cue_track = context.media.track.cue_path.is_some();
    let local = context
        .media
        .track
        .media_uri
        .as_deref()
        .and_then(|uri| uri.strip_prefix("file://"))
        .map(|path| LocalLyricsInput {
            audio_path: PathBuf::from(path),
            title: context.media.track.title.clone(),
            cue_track,
        });
    CurrentResolution {
        input: context.input,
        source: context.source,
        track: context.media.track.clone(),
        local,
        cue_track,
        plan,
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
