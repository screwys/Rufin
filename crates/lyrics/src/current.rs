//! One current lyrics document and its user operations.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use async_channel::Sender;
use library::{
    Libraries, Library, LyricsCacheAuthority, LyricsCacheInput, LyricsCacheKey, LyricsCacheWrite,
    SourceId, Track, TrackId,
};
use playback::{CurrentMedia, CurrentMediaId, SourceSessionEpoch};
use serde::{Deserialize, Serialize};
use sources::{NativeSourceResult, Source, SourceInputIdentity};
use tokio::sync::Semaphore;
use tokio::task::JoinHandle;
use tracing::{debug, warn};

use crate::lyrics::{
    LyricsPlan, cached_lyrics_allowed, embed_lyrics_in_audio, embedded_lyrics_from_audio,
    external_best_lyrics, local_sidecar_lyrics, lyrics_from_edited_text, lyrics_from_native,
    lyrics_to_lrc_text, lyrics_with_displayable_content,
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
    pub loaded: Arc<Library>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct DocumentKey {
    source_id: SourceId,
    source_session_epoch: SourceSessionEpoch,
    track_id: TrackId,
}

impl DocumentKey {
    fn for_context(context: &LyricsContext) -> Self {
        Self {
            source_id: context.media.id.source_id.clone(),
            source_session_epoch: context.media.id.source_session_epoch,
            track_id: context.media.track.id.clone(),
        }
    }

    fn cache_key(&self, plan: &LyricsPlan) -> LyricsCacheKey {
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

    fn cache_key_for(&self, role: LyricsRole, language: &str) -> LyricsCacheKey {
        LyricsCacheKey {
            source_id: self.source_id.clone(),
            track_id: self.track_id.clone(),
            role: role.key().to_string(),
            language: if role == LyricsRole::Translation {
                crate::normalize_language_tag(language).unwrap_or_default()
            } else {
                String::new()
            },
            script: String::new(),
        }
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
    track: Track,
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
    library: Libraries,
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
        library: Libraries,
        runtime: tokio::runtime::Handle,
        settings: Settings,
        private_mode: bool,
        events: Sender<LyricsEvent>,
    ) -> Arc<Self> {
        Arc::new(Self {
            library,
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
        if context.input.source_id != context.media.id.source_id {
            warn!("lyrics context belongs to another source");
            self.clear();
            return;
        }
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
                    current.key.track_id.clone(),
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
                .map(|current| current.context.media.track.id.clone())
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

        // 1. Check embedded lyrics from audio file tags (highest priority)
        if let Some(ref local) = resolution.local {
            let audio_path = local.audio_path.clone();
            let document =
                match tokio::task::spawn_blocking(move || embedded_lyrics_from_audio(&audio_path))
                    .await
                {
                    Ok(doc) => doc,
                    Err(e) => {
                        warn!(%e, "embedded lyrics task panicked");
                        None
                    }
                };
            if let Some(document) = document {
                if document.is_instrumental() || document.has_original() {
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

        // 2. Check sidecar .lrc file
        if let Some(input) = resolution.local.take() {
            let document =
                match tokio::task::spawn_blocking(move || local_sidecar_lyrics(&input)).await {
                    Ok(doc) => doc,
                    Err(e) => {
                        warn!(%e, "sidecar lyrics task panicked");
                        None
                    }
                };
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
            let library = self.library.clone();
            let cache_key = key.cache_key(&resolution.plan);
            let cache_input = cache_input(&resolution.input);
            let cached =
                tokio::task::spawn_blocking(move || library.cached_lyrics(cache_key, cache_input))
                    .await
                    .ok()
                    .and_then(Result::ok)
                    .flatten();
            if let Some(cached) = cached {
                let authority = cached.authority;
                if let Some(document) = cached_bundle(cached)
                    .and_then(lyrics_with_displayable_content)
                    .filter(|document| {
                        cached_lyrics_allowed(document, &resolution.plan, resolution.cue_track)
                            && bundle_satisfies_plan(document)
                    })
                {
                    let is_external = matches!(authority, LyricsCacheAuthority::External);
                    let arc = Arc::new(document);
                    if is_external {
                        self.try_embed_lyrics(&key, &arc).await;
                    }
                    self.accept_bundle(request, &key, arc);
                    return;
                }
                if !self.current_request_active(request, &key, &cancelled) {
                    return;
                }
                let library = self.library.clone();
                let cache_key = key.cache_key(&resolution.plan);
                let _ = tokio::task::spawn_blocking(move || {
                    library.remove_lyrics_if_authority(cache_key, authority)
                })
                .await;
            }
        }
        if !self.current_request_active(request, &key, &cancelled) {
            return;
        }

        if let Some(source) = resolution.source.take() {
            match source
                .lyrics(&key.track_id, resolution.plan.native_search())
                .await
            {
                Ok(NativeSourceResult::Available(Some(native))) => {
                    let document = lyrics_from_native(native);
                    if document.is_instrumental() || document.has_original() {
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
                Ok(NativeSourceResult::Available(None) | NativeSourceResult::Unavailable) => {}
                Err(error) => {
                    debug!(%error, track_id = %key.track_id, "native lyrics request failed");
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
                let selected = document.is_instrumental() || document.has_original();
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
        let is_external = matches!(document.origin, LyricsOrigin::External(_));
        let library = self.library.clone();
        match cache_write(key, input, plan, &document) {
            Ok(write) => {
                match tokio::task::spawn_blocking(move || library.store_lyrics(write)).await {
                    Ok(Ok(_)) => {}
                    Ok(Err(error)) => warn!(%error, "could not save lyrics cache"),
                    Err(error) => warn!(%error, "lyrics cache worker failed"),
                }
            }
            Err(error) => warn!(%error, "could not encode lyrics cache"),
        }
        if is_external {
            self.try_embed_lyrics(key, &document).await;
        }
        self.accept_bundle(request, key, Arc::new(document));
    }

    async fn try_embed_lyrics(&self, key: &DocumentKey, document: &LyricsBundle) {
        {
            let state = self
                .state
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            if !state.settings.auto_embed_lyrics {
                return;
            }
        }
        let Some(first_doc) = document.documents().first() else {
            return;
        };
        let lrc_text = lyrics_to_lrc_text(first_doc, 0);
        if lrc_text.trim().is_empty() {
            return;
        }
        let library = self.library.clone();
        let source_id = key.source_id.clone();
        let track_id = key.track_id.clone();
        let result = tokio::task::spawn_blocking(move || {
            let loaded = library.load_source(&source_id).ok().flatten()?;
            let track = loaded.track(&track_id).ok().flatten()?;
            track.source_path.clone()
        })
        .await;
        match result {
            Ok(Some(path)) => {
                let path = std::path::PathBuf::from(path);
                tokio::task::spawn_blocking(move || {
                    embed_lyrics_in_audio(&path, &lrc_text);
                })
                .await
                .ok();
            }
            _ => {}
        }
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
                    let library = service.library.clone();
                    if let Ok(write) = cache_write(&key, &input, &plan, &document) {
                        let _ =
                            tokio::task::spawn_blocking(move || library.store_lyrics(write)).await;
                    }
                    service.try_embed_lyrics(&key, &document).await;
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
                .track_id
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

    fn update_lyrics_text(self: &Arc<Self>, text: &str) {
        let Some(new_document) = lyrics_from_edited_text(text) else {
            return;
        };
        let event = {
            let mut state = self
                .state
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            let Some(current) = state.current.as_mut() else {
                return;
            };
            current.document = Some(Arc::new(new_document));
            current.request = self.next_request.fetch_add(1, Ordering::AcqRel);
            current_event(current)
        };
        self.publish(event);
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
        let library = self.library.clone();
        let _task = self.runtime.spawn(async move {
            let result = tokio::task::spawn_blocking(move || {
                library.remove_track_lyrics_by_authority(
                    key.source_id,
                    key.track_id,
                    LyricsCacheAuthority::External,
                )
            })
            .await;
            if let Ok(Err(error)) = result {
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

    pub fn update_lyrics_text(&self, text: &str) {
        self.service.update_lyrics_text(text);
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

fn bundle_satisfies_plan(bundle: &LyricsBundle) -> bool {
    bundle.is_instrumental() || bundle.has_original()
}

fn lyrics_selection_changed(previous: &Settings, current: &Settings) -> bool {
    previous.prefer_translations != current.prefer_translations
        || previous.preferred_translation_language != current.preferred_translation_language
}

fn lyrics_acquisition_changed(previous: &Settings, current: &Settings, track_id: &TrackId) -> bool {
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
    let local_file = context
        .loaded
        .sidecar_audio_file(&context.media.track.id)
        .map_err(|error| error.to_string())
        .ok()
        .flatten();
    let cue_track = local_file.as_ref().is_some_and(|audio| audio.cue_track);
    let local = local_file.map(|audio| LocalLyricsInput {
        audio_path: audio.path,
        title: context.media.track.title.clone(),
        cue_track: audio.cue_track,
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

fn cached_bundle(cached: library::CachedLyrics) -> Option<LyricsBundle> {
    decode_cached_bundle(&cached.payload)
}

fn decode_cached_bundle(payload: &str) -> Option<LyricsBundle> {
    let cached = serde_json::from_str::<CachedBundle>(payload).ok()?;
    (cached.version == LYRICS_CACHE_PAYLOAD_VERSION).then_some(cached.bundle)
}

fn cache_write(
    key: &DocumentKey,
    input: &SourceInputIdentity,
    plan: &LyricsPlan,
    document: &LyricsBundle,
) -> Result<LyricsCacheWrite, serde_json::Error> {
    let authority = match document.origin {
        LyricsOrigin::Local | LyricsOrigin::Native => LyricsCacheAuthority::Source,
        LyricsOrigin::External(_) => LyricsCacheAuthority::External,
    };
    Ok(LyricsCacheWrite {
        key: key.cache_key(plan),
        authority,
        input: cache_input(input),
        payload: serde_json::to_string(&CachedBundle {
            version: LYRICS_CACHE_PAYLOAD_VERSION,
            bundle: document.clone(),
        })?,
        cached_at: unix_seconds(),
    })
}

fn cache_input(input: &SourceInputIdentity) -> LyricsCacheInput {
    LyricsCacheInput {
        digest: input.digest,
    }
}

fn unix_seconds() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs().min(i64::MAX as u64) as i64)
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::mpsc;

    use async_channel::{Receiver, unbounded};
    use library::{
        CandidateBatch, CandidateFinish, CandidateHeader, HomeFacts, Libraries, SourceId, Track,
        TrackData, TrackId, TrackRelations,
    };
    use playback::{
        CurrentMedia, CurrentMediaId, OccurrenceId, Provenance, RunId, SourceSessionEpoch,
    };
    use tempfile::TempDir;
    use tokio::runtime::{Builder, Runtime};

    use super::{
        CachedBundle, DocumentKey, LYRICS_CACHE_PAYLOAD_VERSION, LyricsContext, LyricsEvent,
        LyricsService, cache_input, cache_write, current_event, decode_cached_bundle,
        run_external_lookup,
    };
    use crate::{
        CurrentLyrics, CurrentLyricsContent, ExternalLyricsProvider, LyricsBundle, LyricsDocument,
        LyricsLine, LyricsOrigin, LyricsRole, Settings,
    };
    use sources::SourceInputIdentity;

    struct Fixture {
        service: Arc<LyricsService>,
        runtime: Runtime,
        events: Receiver<LyricsEvent>,
        context: LyricsContext,
        _directory: TempDir,
    }

    #[test]
    fn lyrics_owns_and_versions_its_cached_document() {
        let document = LyricsBundle::from_documents(
            LyricsOrigin::External(ExternalLyricsProvider::Lrclib),
            vec![LyricsDocument {
                role: LyricsRole::Original,
                language: None,
                offset_millis: 0,
                lines: vec![lyrics_line("Line", Some(1_000))],
                agents: Vec::new(),
            }],
        );
        let payload = serde_json::to_string(&CachedBundle {
            version: LYRICS_CACHE_PAYLOAD_VERSION,
            bundle: document.clone(),
        })
        .expect("encode cached document");
        assert_eq!(decode_cached_bundle(&payload), Some(document.clone()));

        let instrumental =
            LyricsBundle::instrumental(LyricsOrigin::External(ExternalLyricsProvider::Lrclib));
        let payload = serde_json::to_string(&CachedBundle {
            version: LYRICS_CACHE_PAYLOAD_VERSION,
            bundle: instrumental.clone(),
        })
        .expect("encode cached instrumental state");
        assert_eq!(decode_cached_bundle(&payload), Some(instrumental));

        let incompatible = serde_json::to_string(&CachedBundle {
            version: LYRICS_CACHE_PAYLOAD_VERSION + 1,
            bundle: document,
        })
        .expect("encode incompatible cached document");
        assert_eq!(decode_cached_bundle(&incompatible), None);
    }

    #[test]
    fn current_event_projects_instrumental_without_a_document() {
        let fixture = fixture(Settings::default(), false);
        fixture.service.set_current(Some(fixture.context.clone()));
        drain_events(&fixture.events);
        let event = {
            let mut state = fixture
                .service
                .state
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            let current = state.current.as_mut().expect("current lyrics document");
            current.bundle = Some(Arc::new(LyricsBundle::instrumental(
                LyricsOrigin::External(ExternalLyricsProvider::Lrclib),
            )));
            current_event(current)
        };

        assert!(matches!(
            event,
            LyricsEvent::Current(CurrentLyrics::Ready {
                content: Some(CurrentLyricsContent::Instrumental),
                origin: Some(LyricsOrigin::External(ExternalLyricsProvider::Lrclib)),
                ..
            })
        ));
    }

    #[test]
    fn original_fallback_is_cached_with_plan_key() {
        let fixture = fixture(Settings::default(), false);
        let key = DocumentKey::for_context(&fixture.context);
        let mut settings = Settings::default();
        settings.prefer_translations = true;
        settings.preferred_translation_language = "en".to_string();
        let plan = settings.configured_lyrics_plan(false, &key.track_id);

        let write = cache_write(&key, &fixture.context.input, &plan, &external_document())
            .expect("encode original fallback");

        assert_eq!(write.key.role, LyricsRole::Translation.key());
        assert_eq!(write.key.language, "en");
    }

    #[test]
    fn changing_translation_target_keeps_lyrics_visible_and_uses_the_target_cache() {
        let fixture = fixture(Settings::default(), false);
        let key = DocumentKey::for_context(&fixture.context);
        let mut english = Settings::default();
        english.prefer_translations = true;
        english.preferred_translation_language = "en".to_string();
        let english_bundle = translated_document("en", "English line");
        fixture
            .service
            .library
            .store_lyrics(
                cache_write(
                    &key,
                    &fixture.context.input,
                    &english.configured_lyrics_plan(false, &key.track_id),
                    &english_bundle,
                )
                .expect("encode English translation"),
            )
            .expect("store English translation");

        fixture.service.set_current(Some(fixture.context.clone()));
        drain_events(&fixture.events);
        let visible = Arc::new(external_document().documents()[0].clone());
        {
            let mut state = fixture
                .service
                .state
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            let bundle = Arc::new(external_document());
            let current = state.current.as_mut().expect("current lyrics document");
            current.document = Some(Arc::clone(&visible));
            current.bundle = Some(bundle);
            current.automatic_attempted = true;
        }

        let mut russian = english.clone();
        russian.preferred_translation_language = "ru".to_string();
        fixture.service.settings_changed(russian, false);
        let russian_cancelled = fixture
            .service
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .current_cancelled
            .clone()
            .expect("Russian request cancellation");
        assert!(Arc::ptr_eq(
            fixture
                .service
                .state
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .current
                .as_ref()
                .and_then(|current| current.document.as_ref())
                .expect("visible original"),
            &visible
        ));
        assert_eq!(drain_events(&fixture.events), vec!["ready"]);

        fixture.service.settings_changed(english, false);
        assert!(russian_cancelled.load(Ordering::Acquire));
        assert_eq!(drain_events(&fixture.events), vec!["ready"]);
        drive_current_to_completion(&fixture);

        let state = fixture
            .service
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let document = state
            .current
            .as_ref()
            .and_then(|current| current.document.as_ref())
            .expect("English translation");
        assert_eq!(document.role, LyricsRole::Translation);
        assert_eq!(document.lines[0].text, "English line");
        drop(state);
        assert_eq!(drain_events(&fixture.events), vec!["ready"]);
    }

    #[test]
    fn missing_translation_keeps_the_visible_original() {
        let mut settings = Settings::default();
        settings.external_lyrics_enabled = false;
        let fixture = fixture(settings.clone(), false);
        fixture.service.set_current(Some(fixture.context.clone()));
        drain_events(&fixture.events);
        let bundle = Arc::new(external_document());
        let visible = Arc::new(bundle.documents()[0].clone());
        {
            let mut state = fixture
                .service
                .state
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            let current = state.current.as_mut().expect("current lyrics document");
            current.document = Some(Arc::clone(&visible));
            current.bundle = Some(bundle);
            current.automatic_attempted = true;
        }

        settings.prefer_translations = true;
        settings.preferred_translation_language = "ru".to_string();
        fixture.service.settings_changed(settings, false);
        drive_current_to_completion(&fixture);

        let state = fixture
            .service
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let current = state.current.as_ref().expect("current lyrics document");
        assert!(!current.loading);
        assert!(Arc::ptr_eq(
            current.document.as_ref().expect("visible original"),
            &visible
        ));
        drop(state);
        assert_eq!(drain_events(&fixture.events), vec!["ready", "ready"]);
    }

    #[test]
    fn missing_translation_keeps_the_instrumental_result() {
        let mut settings = Settings::default();
        settings.external_lyrics_enabled = false;
        let fixture = fixture(settings.clone(), false);
        fixture.service.set_current(Some(fixture.context.clone()));
        drain_events(&fixture.events);
        {
            let mut state = fixture
                .service
                .state
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            let current = state.current.as_mut().expect("current lyrics document");
            current.bundle = Some(Arc::new(LyricsBundle::instrumental(
                LyricsOrigin::External(ExternalLyricsProvider::Lrclib),
            )));
            current.automatic_attempted = true;
        }

        settings.prefer_translations = true;
        settings.preferred_translation_language = "ru".to_string();
        fixture.service.settings_changed(settings, false);
        drive_current_to_completion(&fixture);

        let state = fixture
            .service
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        assert!(
            state
                .current
                .as_ref()
                .and_then(|current| current.bundle.as_ref())
                .is_some_and(|bundle| bundle.is_instrumental())
        );
    }

    #[test]
    fn private_mode_keeps_the_current_external_document() {
        let fixture = fixture(Settings::default(), false);
        fixture.service.set_current(Some(fixture.context.clone()));
        drain_events(&fixture.events);
        let bundle = Arc::new(external_document());
        let document = Arc::new(bundle.documents()[0].clone());
        {
            let mut state = fixture
                .service
                .state
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            let current = state.current.as_mut().expect("current lyrics document");
            current.document = Some(Arc::clone(&document));
            current.bundle = Some(bundle);
            current.automatic_attempted = true;
        }

        fixture.service.settings_changed(Settings::default(), true);

        let state = fixture
            .service
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let current = state.current.as_ref().expect("current lyrics document");
        assert_eq!(current.document.as_deref(), Some(document.as_ref()));
        assert!(current.automatic_attempted);
        assert!(!current.loading);
        assert!(state.private_mode);
        drop(state);
        assert!(drain_events(&fixture.events).is_empty());
    }

    #[test]
    fn private_mode_loads_the_external_cache_without_deleting_it() {
        let fixture = fixture(Settings::default(), true);
        let key = DocumentKey::for_context(&fixture.context);
        let document = external_document();
        let plan = Settings::default().configured_lyrics_plan(true, &key.track_id);
        fixture
            .service
            .library
            .store_lyrics(
                cache_write(&key, &fixture.context.input, &plan, &document)
                    .expect("encode cached lyrics"),
            )
            .expect("store cached lyrics");
        fixture.service.set_current(Some(fixture.context.clone()));
        drain_events(&fixture.events);

        fixture
            .service
            .handle()
            .load(fixture.context.media.id.clone());
        drive_current_to_completion(&fixture);

        let state = fixture
            .service
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let current = state.current.as_ref().expect("current lyrics document");
        assert_eq!(current.document.as_deref(), document.documents().first());
        drop(state);
        assert!(
            fixture
                .service
                .library
                .cached_lyrics(key.cache_key(&plan), cache_input(&fixture.context.input))
                .expect("read cached lyrics")
                .is_some()
        );
        assert_eq!(drain_events(&fixture.events), vec!["loading", "ready"]);
    }

    #[test]
    fn private_mode_cancels_loading_and_starts_one_source_only_resolution() {
        let fixture = fixture(Settings::default(), false);
        fixture.service.set_current(Some(fixture.context.clone()));
        drain_events(&fixture.events);
        let cancelled = Arc::new(AtomicBool::new(false));
        {
            let mut state = fixture
                .service
                .state
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            let current = state.current.as_mut().expect("current lyrics document");
            current.automatic_attempted = true;
            current.loading = true;
            state.current_cancelled = Some(Arc::clone(&cancelled));
        }

        fixture.service.settings_changed(Settings::default(), true);
        assert!(cancelled.load(Ordering::Acquire));
        drive_current_to_completion(&fixture);

        assert_eq!(
            drain_events(&fixture.events),
            vec!["loading", "ready"],
            "the settings owner starts one replacement request"
        );
    }

    #[test]
    fn settings_keep_a_never_requested_current_document_lazy() {
        let fixture = fixture(Settings::default(), false);
        fixture.service.set_current(Some(fixture.context.clone()));
        drain_events(&fixture.events);
        let request = fixture
            .service
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .current
            .as_ref()
            .expect("current lyrics document")
            .request;
        let mut changed = Settings::default();
        changed.prefer_server_lyrics = false;

        fixture.service.settings_changed(changed, false);

        let state = fixture
            .service
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let current = state.current.as_ref().expect("current lyrics document");
        assert_eq!(current.request, request);
        assert!(!current.automatic_attempted);
        assert!(!current.loading);
        assert!(state.current_cancelled.is_none());
        assert!(state.current_task.is_none());
        drop(state);
        assert!(drain_events(&fixture.events).is_empty());
    }

    #[test]
    fn provider_settings_keep_the_visible_document() {
        let fixture = fixture(Settings::default(), false);
        fixture.service.set_current(Some(fixture.context.clone()));
        drain_events(&fixture.events);
        let bundle = Arc::new(external_document());
        let document = Arc::new(bundle.documents()[0].clone());
        let request = {
            let mut state = fixture
                .service
                .state
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            let current = state.current.as_mut().expect("current lyrics document");
            current.document = Some(Arc::clone(&document));
            current.bundle = Some(bundle);
            current.automatic_attempted = true;
            current.request
        };
        let mut changed = Settings::default();
        changed
            .external_lyrics_providers
            .push(ExternalLyricsProvider::Genius);

        fixture.service.settings_changed(changed, false);

        let state = fixture
            .service
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let current = state.current.as_ref().expect("current lyrics document");
        assert_eq!(current.request, request);
        assert!(Arc::ptr_eq(
            current.document.as_ref().expect("visible lyrics"),
            &document
        ));
        assert!(!current.loading);
        assert!(state.current_cancelled.is_none());
        assert!(state.current_task.is_none());
        drop(state);
        assert!(drain_events(&fixture.events).is_empty());
    }

    #[test]
    fn repeated_playback_projection_does_not_republish_current_lyrics() {
        let fixture = fixture(Settings::default(), false);
        fixture.service.set_current(Some(fixture.context.clone()));
        assert_eq!(drain_events(&fixture.events), vec!["ready"]);

        let mut repeated = fixture.context.clone();
        repeated.media = Arc::new((*fixture.context.media).clone());
        assert!(!Arc::ptr_eq(&repeated.media, &fixture.context.media));
        fixture.service.set_current(Some(repeated.clone()));

        assert!(
            drain_events(&fixture.events).is_empty(),
            "position projections must not rebuild the visible lyrics document"
        );
        let state = fixture
            .service
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let current = state.current.as_ref().expect("current lyrics document");
        assert!(Arc::ptr_eq(&current.context.media, &repeated.media));
        drop(state);

        let mut next_media = (*repeated.media).clone();
        next_media.id.run = Some(RunId::new(2));
        repeated.media = Arc::new(next_media);
        fixture.service.set_current(Some(repeated));
        assert_eq!(
            drain_events(&fixture.events),
            vec!["ready"],
            "a new current-media identity must still replace the visible projection"
        );
    }

    #[test]
    fn cancelled_current_lookup_keeps_the_external_lane_until_work_finishes() {
        let runtime = Builder::new_current_thread()
            .build()
            .expect("external lyrics runtime");
        let lane = Arc::new(tokio::sync::Semaphore::new(1));
        let (started, work_started) = tokio::sync::oneshot::channel();
        let (release, work_release) = mpsc::sync_channel(0);
        let task = runtime.spawn(run_external_lookup(Arc::clone(&lane), move || {
            started.send(()).expect("publish external lookup start");
            work_release.recv().expect("release external lookup");
            Ok(None)
        }));
        runtime
            .block_on(work_started)
            .expect("external lookup started");

        task.abort();
        assert!(
            runtime
                .block_on(task)
                .expect_err("the current lookup task was cancelled")
                .is_cancelled()
        );
        assert_eq!(
            lane.available_permits(),
            0,
            "cancelling the async owner must not admit overlapping blocking work"
        );

        release.send(()).expect("finish external lookup");
        let permit = runtime
            .block_on(Arc::clone(&lane).acquire_owned())
            .expect("the external lane remains open");
        drop(permit);
    }

    fn fixture(settings: Settings, private_mode: bool) -> Fixture {
        let directory = tempfile::tempdir().expect("temporary lyrics Store");
        let library =
            Libraries::open(directory.path().join("library.db")).expect("open lyrics Store");
        let source_id = SourceId::new("local:lyrics-current");
        let track = track();
        let mut candidate = library
            .begin_source_candidate(CandidateHeader {
                source_id: source_id.clone(),
                input_digest: [1; 32],
            })
            .expect("begin lyrics source");
        candidate
            .write(CandidateBatch::Tracks(vec![track.clone()]))
            .expect("write lyrics Track");
        let loaded = candidate
            .finish(
                CandidateFinish {
                    freshness: None,
                    home: HomeFacts::RufinDefined,
                    accepted_at: 1,
                },
                None,
            )
            .and_then(library::PreparedSourceCandidate::accept)
            .expect("accept lyrics source")
            .library;
        let runtime = Builder::new_current_thread()
            .build()
            .expect("lyrics test runtime");
        let (events, receiver) = unbounded();
        let service = LyricsService::new(
            library,
            runtime.handle().clone(),
            settings,
            private_mode,
            events,
        );
        let media = Arc::new(CurrentMedia {
            id: CurrentMediaId {
                source_id: source_id.clone(),
                source_session_epoch: SourceSessionEpoch::new(1),
                run: Some(RunId::new(1)),
                occurrence: OccurrenceId::new("lyrics-current"),
            },
            track,
            provenance: Provenance::Manual,
        });
        let context = LyricsContext {
            media,
            input: SourceInputIdentity {
                source_id,
                digest: [1; 32],
            },
            source: None,
            loaded,
        };
        Fixture {
            service,
            runtime,
            events: receiver,
            context,
            _directory: directory,
        }
    }

    fn track() -> Track {
        Track::new(TrackData {
            id: TrackId::new("track:lyrics-current"),
            album_id: None,
            title: "Track".to_string(),
            artist: "Artist".to_string(),
            album: "Album".to_string(),
            album_artwork: None,
            year: 2026,
            release_date: None,
            date_added: None,
            last_played: None,
            play_count: None,
            user_rating: None,
            duration_seconds: 180,
            favorite: false,
            disc_number: 1,
            track_number: 1,
            image_ref: None,
            local_artwork: None,
            musicbrainz_recording_id: None,
            musicbrainz_release_track_id: None,
            source_path: None,
            cue: None,
            source_format: None,
            comment: None,
            skip_count: None,
            bpm: None,
            relations: TrackRelations::default(),
        })
    }

    fn external_document() -> LyricsBundle {
        LyricsBundle::from_documents(
            LyricsOrigin::External(ExternalLyricsProvider::Lrclib),
            vec![LyricsDocument {
                role: LyricsRole::Original,
                language: None,
                offset_millis: 0,
                lines: vec![lyrics_line("Cached line", Some(1_000))],
                agents: Vec::new(),
            }],
        )
    }

    fn translated_document(language: &str, text: &str) -> LyricsBundle {
        let mut bundle = external_document();
        bundle.documents_mut().push(LyricsDocument {
            role: LyricsRole::Translation,
            language: Some(language.to_string()),
            offset_millis: 0,
            lines: vec![lyrics_line(text, Some(1_000))],
            agents: Vec::new(),
        });
        bundle
    }

    fn lyrics_line(text: &str, start_millis: Option<u64>) -> LyricsLine {
        LyricsLine {
            text: text.to_string(),
            start_millis,
            end_millis: None,
            cue_lines: Vec::new(),
        }
    }

    fn drive_current_to_completion(fixture: &Fixture) {
        let task = {
            let mut state = fixture
                .service
                .state
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            if !state
                .current
                .as_ref()
                .is_some_and(|current| current.loading)
            {
                return;
            }
            state
                .current_task
                .take()
                .expect("loading lyrics must retain their resolution task")
        };
        fixture
            .runtime
            .block_on(task)
            .expect("lyrics resolution task must complete");

        let state = fixture
            .service
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let current = state.current.as_ref().expect("current lyrics document");
        assert!(
            !current.loading,
            "lyrics resolution task completed without finishing request {}",
            current.request
        );
    }

    fn drain_events(events: &Receiver<LyricsEvent>) -> Vec<&'static str> {
        let mut drained = Vec::new();
        while let Ok(event) = events.try_recv() {
            if let LyricsEvent::Current(current) = event {
                drained.push(match current {
                    CurrentLyrics::Cleared => "cleared",
                    CurrentLyrics::Loading { .. } => "loading",
                    CurrentLyrics::Ready { .. } => "ready",
                });
            }
        }
        drained
    }
}
