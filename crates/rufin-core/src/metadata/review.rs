use super::{
    MetadataBatch, MetadataCandidate, MetadataEditor, MetadataMatchKind, MetadataTrackSummary,
};
use localization::{msgid, tr};
use metadata_lookup::{CacheMode, LookupCache};
use sources::{SourceEntityKind, SourceMetadataError as Error};
use std::sync::{Arc, Mutex};
use tokio_util::sync::CancellationToken;

#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
#[derive(Clone)]
pub struct MetadataLookupProgress {
    pub completed: u64,
    pub cached: u64,
    pub restoring_cache: bool,
    pub total: u64,
    pub finished: bool,
    pub failure: Option<String>,
    pub active_track: Option<u64>,
    pub stage: String,
}

pub(super) type LookupProgress = Arc<dyn Fn(&str) + Send + Sync>;

#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
#[derive(Clone, PartialEq, Eq)]
pub struct MetadataLookupField {
    pub label: String,
    pub original: String,
    pub suggested: String,
    pub changed: bool,
}

#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
#[derive(Clone, PartialEq, Eq)]
pub struct MetadataLookupItem {
    pub title: String,
    pub ready: bool,
    pub candidates: Vec<MetadataCandidate>,
    pub selected: Option<String>,
    pub fields: Vec<MetadataLookupField>,
    pub failure: Option<String>,
}

enum Items {
    Single {
        original: Arc<MetadataEditor>,
        working: Arc<MetadataEditor>,
    },
    Batch(Arc<MetadataBatch>),
}

#[cfg_attr(feature = "bindings", derive(uniffi::Object))]
pub struct MetadataLookupReview {
    items: Items,
    progress: Mutex<MetadataLookupProgress>,
    updates: (async_channel::Sender<()>, async_channel::Receiver<()>),
    cancelled: CancellationToken,
    run_lock: tokio::sync::Mutex<()>,
}

#[cfg_attr(feature = "bindings", uniffi::export)]
impl MetadataLookupReview {
    #[cfg_attr(feature = "bindings", uniffi::constructor)]
    pub async fn open(editor: Arc<MetadataEditor>) -> Result<Arc<Self>, Error> {
        let runtime = editor.source.shared.runtime.clone();
        super::on_runtime(runtime, async {
            editor.stage_changes().await?;
            if let Some(scope) = &editor.batch {
                return Self::open_batch(
                    scope.batch.clone(),
                    scope.indices.iter().map(|i| *i as u64).collect(),
                )
                .await;
            }
            let mut state = editor.state().clone();
            let mut draft = editor.draft.as_ref().clone();
            state.apply_values(&mut draft);
            state.begin_lookup();
            let working = Arc::new(MetadataEditor {
                draft: Arc::new(draft),
                source: editor.source.clone(),
                session: editor.session.clone(),
                batch: None,
                state: Mutex::new(state),
            });
            Ok(Self::new(
                Items::Single {
                    original: editor,
                    working,
                },
                1,
            ))
        })
        .await
    }

    #[cfg_attr(feature = "bindings", uniffi::constructor)]
    pub async fn open_batch(
        batch: Arc<MetadataBatch>,
        indices: Vec<u64>,
    ) -> Result<Arc<Self>, Error> {
        let runtime = batch.source.shared.runtime.clone();
        super::on_runtime(runtime, async {
            let indices = if indices.is_empty() {
                (0..batch.track_count() as usize).collect()
            } else {
                indices
                    .into_iter()
                    .map(|index| usize::try_from(index).map_err(|_| Error::Unavailable))
                    .collect::<Result<Vec<_>, _>>()?
            };
            if indices
                .iter()
                .any(|index| *index >= batch.track_count() as usize)
            {
                return Err(Error::Unavailable);
            }
            let count = indices.len() as u64;
            Ok(Self::new(Items::Batch(batch.fork(indices)?), count))
        })
        .await
    }

    pub fn progress(&self) -> MetadataLookupProgress {
        self.progress
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    pub async fn next_update(&self) -> Option<MetadataLookupProgress> {
        let runtime = self.source().shared.runtime.clone();
        super::on_runtime(runtime, async {
            self.updates.1.recv().await.ok().map(|_| self.progress())
        })
        .await
    }

    pub fn scope_title(&self) -> Option<String> {
        match &self.items {
            Items::Batch(batch) => batch.scope_title(),
            _ => None,
        }
    }

    pub fn cancel(&self) {
        self.cancelled.cancel();
    }

    pub async fn run(self: Arc<Self>) {
        let runtime = self.source().shared.runtime.clone();
        super::on_runtime(runtime, async {
        let _run = self.run_lock.lock().await;
        if self.cancelled.is_cancelled() {
            return;
        }
        {
            let mut progress = self.progress.lock().unwrap_or_else(|e| e.into_inner());
            progress.completed = 0;
            progress.cached = 0;
            progress.restoring_cache = true;
            progress.finished = false;
            progress.failure = None;
        }
        let count = self.progress().total;
        let mut pending = Vec::new();
        self.report(0, &tr(msgid("Loading cached lookups...")));
        for start in (0..count).step_by(512) {
            if self.cancelled.is_cancelled() {
                break;
            }
            let end = count.min(start + 512);
            let uris = (start..end)
                .map(|index| self.uri(index).to_string())
                .collect::<Vec<_>>();
            let cached = self
                .source()
                .shared
                .database
                .cached_fingerprint_lookups(&uris, metadata_lookup::cache_time())
                .await;
            let cached = match cached {
                Ok(cached) => cached.into_iter().collect::<std::collections::HashSet<_>>(),
                Err(error) => {
                    self.progress
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .failure = Some(error.to_string());
                    break;
                }
            };
            for index in start..end {
                if self.cancelled.is_cancelled() {
                    break;
                }
                if !self
                    .source()
                    .shared
                    .settings
                    .load()
                    .ui
                    .allows_external_metadata_lookup()
                {
                    self.progress
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .failure = Some(Error::Unavailable.to_string());
                    self.cancelled.cancel();
                    break;
                }
                let outcome = if cached.contains(self.uri(index)) {
                    tokio::select! {
                        _ = self.cancelled.cancelled() => break,
                        result = self.discover(index, String::new(), Arc::new(|_| {}), CacheMode::Only) => result,
                    }
                } else {
                    Ok(Discovery::Missing)
                };
                {
                    let mut progress = self.progress.lock().unwrap_or_else(|e| e.into_inner());
                    progress.active_track = Some(index);
                    if matches!(outcome, Ok(Discovery::Cached)) {
                        progress.completed += 1;
                        progress.cached += 1;
                    } else {
                        pending.push(index);
                    }
                }
                if (index + 1) % 64 == 0 {
                    let _ = self.updates.0.try_send(());
                }
            }
        }
        self.progress
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .restoring_cache = false;
        let _ = self.updates.0.try_send(());
        for index in pending {
            if self.cancelled.is_cancelled() || self.progress().failure.is_some() {
                break;
            }
            if !self
                .source()
                .shared
                .settings
                .load()
                .ui
                .allows_external_metadata_lookup()
            {
                self.progress
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .failure = Some(Error::Unavailable.to_string());
                break;
            }
            let owner = Arc::downgrade(&self);
            let report: LookupProgress = Arc::new(move |stage| {
                if let Some(owner) = owner.upgrade() {
                    owner.report(index, stage);
                }
            });
            let outcome = tokio::select! {
                _ = self.cancelled.cancelled() => break,
                result = self.discover(index, String::new(), report, CacheMode::Use) => result,
            };
            let mut progress = self.progress.lock().unwrap_or_else(|e| e.into_inner());
            progress.completed += 1;
            match outcome {
                Ok(Discovery::Cached) => progress.cached += 1,
                Err(error) => {
                    progress.failure = Some(error.to_string());
                    break;
                }
                _ => {}
            }
            drop(progress);
            let _ = self.updates.0.try_send(());
        }
        {
            let mut progress = self.progress.lock().unwrap_or_else(|e| e.into_inner());
            progress.finished = true;
            progress.active_track = None;
            progress.stage.clear();
        }
        let _ = self.updates.0.try_send(());
            }).await
    }

    pub async fn tracks(
        &self,
        offset: u64,
        limit: u64,
    ) -> Result<Vec<MetadataTrackSummary>, Error> {
        let runtime = self.source().shared.runtime.clone();
        super::on_runtime(runtime, async {
            let mut tracks = match &self.items {
                Items::Batch(batch) => batch.clone().tracks(offset, limit).await,
                Items::Single { working, .. } if offset == 0 && limit > 0 => {
                    let snapshot = working.snapshot();
                    let state = working.state();
                    Ok(vec![MetadataTrackSummary {
                        index: 0,
                        title: title_values(&state),
                        modified: snapshot.has_changes,
                        error: state.scan_error.clone(),
                        scan_status: lookup_status(&state),
                    }])
                }
                _ => Ok(Vec::new()),
            }?;
            let progress = self.progress();
            if let Some(row) = tracks
                .iter_mut()
                .find(|row| Some(row.index) == progress.active_track)
            {
                row.scan_status = progress.stage;
            }
            Ok(tracks)
        })
        .await
    }

    pub async fn item(&self, index: u64) -> Result<MetadataLookupItem, Error> {
        let runtime = self.source().shared.runtime.clone();
        super::on_runtime(runtime, async {
            let editor = self.editor(index, false).await?;
            let mut state = editor.state().clone();
            state.apply_lookup_choice(&editor.draft, false);
            let fields = state.fields(&editor.draft);
            Ok(MetadataLookupItem {
                title: title_values(&state),
                ready: state.lookup_candidates.is_some(),
                candidates: state.lookup_candidates.clone().unwrap_or_default(),
                selected: state
                    .lookup_choice
                    .as_ref()
                    .map(|(candidate, _)| candidate.id.clone()),
                fields: fields
                    .into_iter()
                    .map(|field| MetadataLookupField {
                        changed: field.original_value != field.field.value,
                        label: field.field.label,
                        original: field.original_value,
                        suggested: field.field.value,
                    })
                    .collect(),
                failure: state.scan_error.clone(),
            })
        })
        .await
    }

    pub async fn choose(
        &self,
        index: u64,
        candidate: Option<MetadataCandidate>,
    ) -> Result<(), Error> {
        let runtime = self.source().shared.runtime.clone();
        super::on_runtime(runtime, async {
            let editor = self.editor(index, false).await?;
            let changes = if let Some(candidate) = &candidate {
                match self
                    .resolve_candidate(
                        &editor,
                        candidate.clone(),
                        editor.lookup_cache(CacheMode::Use),
                    )
                    .await
                {
                    Ok(changes) => Some(changes.ok_or(Error::Unavailable)?),
                    Err(error) => {
                        editor.state().scan_error = Some(error.to_string());
                        self.store(index, &editor).await?;
                        return Err(error);
                    }
                }
            } else {
                None
            };
            {
                let mut state = editor.state();
                state.lookup_choice = candidate.zip(changes);
                state.scan_error = None;
            }
            self.store(index, &editor).await
        })
        .await
    }

    pub async fn search(&self, index: u64, query: String) -> Result<(), Error> {
        let runtime = self.source().shared.runtime.clone();
        super::on_runtime(runtime, async {
            let result = self
                .discover(index, query, Arc::new(|_| {}), CacheMode::Use)
                .await;
            if let Err(error) = &result {
                let editor = self.editor(index, false).await?;
                editor.state().scan_error = Some(error.to_string());
                self.store(index, &editor).await?;
            }
            result.map(|_| ())
        })
        .await
    }

    pub async fn refresh(&self, index: u64, query: String) -> Result<(), Error> {
        let runtime = self.source().shared.runtime.clone();
        super::on_runtime(runtime, async {
            self.discover(index, query, Arc::new(|_| {}), CacheMode::Refresh)
                .await
                .map(|_| ())
        })
        .await
    }

    pub async fn accept(&self, fill_missing: bool) -> Result<(), Error> {
        let runtime = self.source().shared.runtime.clone();
        super::on_runtime(runtime, async {
            self.cancel();
            let _run = self.run_lock.lock().await;
            match &self.items {
                Items::Batch(batch) => batch.accept_lookup(fill_missing).await,
                Items::Single { original, working } => {
                    let mut state = working.state().clone();
                    if state.lookup_choice.is_some() {
                        state.apply_lookup_choice(&working.draft, fill_missing);
                        *original.state() = state;
                    }
                    Ok(())
                }
            }
        })
        .await
    }
}

impl MetadataLookupReview {
    fn new(items: Items, total: u64) -> Arc<Self> {
        Arc::new(Self {
            items,
            progress: Mutex::new(MetadataLookupProgress {
                completed: 0,
                cached: 0,
                restoring_cache: true,
                total,
                finished: false,
                failure: None,
                active_track: None,
                stage: String::new(),
            }),
            updates: async_channel::bounded(1),
            cancelled: CancellationToken::new(),
            run_lock: tokio::sync::Mutex::new(()),
        })
    }

    fn report(&self, index: u64, stage: &str) {
        if self.cancelled.is_cancelled() {
            return;
        }
        {
            let mut progress = self.progress.lock().unwrap_or_else(|e| e.into_inner());
            if progress.finished {
                return;
            }
            progress.active_track = Some(index);
            progress.stage = stage.to_string();
        }
        let _ = self.updates.0.try_send(());
    }

    async fn editor(&self, index: u64, scan: bool) -> Result<Arc<MetadataEditor>, Error> {
        if index >= self.progress().total {
            return Err(Error::Unavailable);
        }
        match &self.items {
            Items::Batch(batch) => batch.detached_editor(index as usize, scan).await,
            Items::Single { working, .. } if index == 0 => Ok(working.clone()),
            _ => Err(Error::Unavailable),
        }
    }

    async fn store(&self, index: u64, editor: &MetadataEditor) -> Result<(), Error> {
        if let Items::Batch(batch) = &self.items {
            batch.store_editor(index as usize, editor).await?;
        }
        Ok(())
    }

    async fn discover(
        &self,
        index: u64,
        query: String,
        report: LookupProgress,
        mode: CacheMode,
    ) -> Result<Discovery, Error> {
        report(&tr(msgid("Reading tags...")));
        let mut editor = self.editor(index, false).await?;
        if mode != CacheMode::Refresh
            && query.is_empty()
            && editor.state().lookup_candidates.is_some()
        {
            return Ok(Discovery::Cached);
        }
        let cache = editor.lookup_cache(mode);
        let mut candidates = Vec::new();
        let mut failure = None;
        if query.is_empty() && editor.draft.kind == SourceEntityKind::Track {
            match self.editor(index, true).await {
                Ok(scanning) => {
                    editor = scanning;
                    match editor.clone().scan(report.clone(), cache.clone()).await {
                        Ok(Some(found)) => candidates = found,
                        Ok(None) => return Ok(Discovery::Missing),
                        Err(error) => failure = Some(error.to_string()),
                    }
                }
                Err(error) => failure = Some(error.to_string()),
            }
        }
        if candidates.is_empty() {
            report(&tr(msgid("Searching MusicBrainz...")));
            let Some(found) = self
                .search_candidates(&editor, query.clone(), cache.clone())
                .await?
            else {
                return Ok(Discovery::Missing);
            };
            candidates = found;
        }
        if query.is_empty()
            && editor.draft.kind != SourceEntityKind::Track
            && mode != CacheMode::Only
        {
            match self
                .identify_entity(&editor, report.clone(), cache.clone())
                .await
            {
                Ok(found) => {
                    for candidate in found {
                        if !candidates.iter().any(|current| current.id == candidate.id) {
                            candidates.push(candidate);
                        }
                    }
                }
                Err(error) if candidates.is_empty() => failure = Some(error.to_string()),
                Err(_) => {}
            }
        }
        if !candidates.is_empty() {
            failure = None;
        }
        let chosen = {
            let state = editor.state();
            let release = state.values.get("musicbrainz_album_id");
            candidates
                .iter()
                .find(|candidate| release == Some(&candidate.id))
                .cloned()
                .or_else(|| {
                    let recordings = &state.recordings;
                    if candidates
                        .iter()
                        .any(|candidate| candidate.from_fingerprint)
                        && recordings.len() == 1
                    {
                        candidates
                            .iter()
                            .find(|candidate| candidate.kind == MetadataMatchKind::Recording)
                            .cloned()
                    } else if candidates.len() == 1 {
                        candidates.first().cloned()
                    } else {
                        None
                    }
                })
        };
        let resolved = if let Some(chosen) = chosen {
            report(&tr(msgid("Fetching metadata...")));
            match self
                .resolve_candidate(&editor, chosen.clone(), cache.clone())
                .await
            {
                Ok(Some(changes)) => Some((chosen, changes)),
                Ok(None) => return Ok(Discovery::Missing),
                Err(Error::InvalidInput(error)) => {
                    failure = Some(error);
                    None
                }
                Err(error) => return Err(error),
            }
        } else {
            None
        };
        {
            let mut state = editor.state();
            state.lookup_choice = resolved;
            state.scan_error = failure;
            state.lookup_candidates = Some(candidates);
        }
        self.store(index, &editor).await?;
        Ok(if cache.used_cache() && !cache.used_network() {
            Discovery::Cached
        } else {
            Discovery::Fetched
        })
    }
}

impl MetadataLookupReview {
    fn source(&self) -> &Arc<crate::source::SourceOwner> {
        match &self.items {
            Items::Batch(batch) => &batch.source,
            Items::Single { working, .. } => &working.source,
        }
    }

    fn uri(&self, index: u64) -> &str {
        match &self.items {
            Items::Batch(batch) => batch.uri(index as usize),
            Items::Single { working, .. } => &working.session.as_ref().unwrap().media_uri,
        }
    }

    async fn search_candidates(
        &self,
        editor: &MetadataEditor,
        query: String,
        cache: Arc<LookupCache>,
    ) -> Result<Option<Vec<MetadataCandidate>>, Error> {
        if !editor
            .source
            .shared
            .settings
            .load()
            .ui
            .allows_external_metadata_lookup()
        {
            return Ok(Some(Vec::new()));
        }
        let values = {
            let state = editor.state();
            let mut values = state.values();
            for field in &state.mixed {
                values.remove(field);
            }
            values
        };
        let kind = editor.draft.kind;
        if query.trim().is_empty()
            && let Some(candidate) = metadata_lookup::metadata_identity(kind, &values)
        {
            return Ok(Some(vec![candidate]));
        }
        editor
            .source
            .shared
            .runtime
            .spawn_blocking(move || metadata_lookup::search_metadata(&cache, kind, &values, &query))
            .await
            .map_err(|e| Error::Write(e.to_string()))?
            .map_err(Error::Write)
    }

    async fn identify_entity(
        &self,
        editor: &MetadataEditor,
        report: LookupProgress,
        cache: Arc<LookupCache>,
    ) -> Result<Vec<MetadataCandidate>, Error> {
        let rows = editor
            .source
            .shared
            .database
            .file_edit_rows(
                &editor.session.as_ref().ok_or(Error::Unavailable)?.media_uri,
                None,
                &library::ReadCancellation::new(),
            )
            .await
            .map_err(|e| Error::Write(e.to_string()))?;
        let count = rows.len().min(3);
        let mut found = Vec::new();
        for (index, row) in rows.into_iter().take(count).enumerate() {
            let outer = report.clone();
            let progress: LookupProgress = Arc::new(move |stage| {
                outer(&localization::tr_with(
                    "{stage} ({count} of {total} tracks)",
                    &[
                        ("stage", stage),
                        ("count", &(index + 1).to_string()),
                        ("total", &count.to_string()),
                    ],
                ))
            });
            let track = MetadataEditor::open(
                editor.source.clone(),
                super::MetadataItemId::Track(row.media_uri),
            )
            .await?;
            let Some(matches) = track.scan(progress, cache.clone()).await? else {
                continue;
            };
            if editor.draft.kind == SourceEntityKind::Album {
                found.extend(
                    matches
                        .into_iter()
                        .filter(|candidate| candidate.kind == super::MetadataMatchKind::Release),
                );
            } else {
                for recording in matches
                    .into_iter()
                    .filter(|candidate| candidate.kind == super::MetadataMatchKind::Recording)
                {
                    let request = recording.clone();
                    let cache = cache.clone();
                    let lookup = editor
                        .source
                        .shared
                        .runtime
                        .spawn_blocking(move || metadata_lookup::lookup_metadata(&cache, &request))
                        .await
                        .map_err(|e| Error::Write(e.to_string()))?
                        .map_err(Error::Write)?;
                    if let Some(lookup) = lookup {
                        found.extend(lookup.artists());
                    }
                }
            }
        }
        let mut ids = std::collections::HashSet::new();
        found.retain(|candidate| ids.insert(candidate.id.clone()));
        Ok(found)
    }

    async fn resolve_candidate(
        &self,
        editor: &MetadataEditor,
        candidate: MetadataCandidate,
        cache: Arc<LookupCache>,
    ) -> Result<Option<sources::MetadataChanges>, Error> {
        if !editor
            .source
            .shared
            .settings
            .load()
            .ui
            .allows_external_metadata_lookup()
        {
            return Err(Error::Unavailable);
        }
        if cache.mode != CacheMode::Refresh {
            let state = editor.state();
            if let Some((cached, changes)) = &state.lookup_choice
                && cached == &candidate
            {
                return Ok(Some(changes.clone()));
            }
        }
        let request = candidate.clone();
        let lookup = editor
            .source
            .shared
            .runtime
            .spawn_blocking(move || metadata_lookup::lookup_metadata(&cache, &request))
            .await
            .map_err(|e| Error::Write(e.to_string()))?
            .map_err(Error::Write)?;
        let Some(lookup) = lookup else {
            return Ok(None);
        };

        let state = editor.state();
        lookup
            .changes(
                editor.draft.kind,
                &state.values(),
                if candidate.from_fingerprint {
                    &state.recordings
                } else {
                    &[]
                },
            )
            .map(Some)
            .ok_or_else(|| Error::InvalidInput(tr(msgid("No matching track in this release"))))
    }
}

fn title_values(state: &super::editor::Editing) -> String {
    state.values.get("title").cloned().unwrap_or_default()
}

pub(super) fn lookup_status(state: &super::editor::Editing) -> String {
    match &state.lookup_candidates {
        None => String::new(),
        Some(_) if state.lookup_choice.is_some() => localization::tr("Matched"),
        Some(candidates) if candidates.is_empty() => localization::tr("No results"),
        Some(_) => localization::tr("Needs review"),
    }
}

#[derive(Clone, Copy)]
enum Discovery {
    Missing,
    Cached,
    Fetched,
}
