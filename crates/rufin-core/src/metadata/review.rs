use super::{
    MetadataBatch, MetadataCandidate, MetadataEditor, MetadataMatchKind, MetadataTrackSummary,
};
use localization::{msgid, tr};
use sources::{MetadataDraft, SourceEntityKind, SourceMetadataError as Error};
use std::sync::{Arc, Mutex};
use tokio_util::sync::CancellationToken;

#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
#[derive(Clone)]
pub struct MetadataLookupProgress {
    pub completed: u64,
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
    response: Mutex<Option<(MetadataCandidate, Arc<metadata_lookup::MetadataLookup>)>>,
    progress: Mutex<MetadataLookupProgress>,
    updates: (async_channel::Sender<()>, async_channel::Receiver<()>),
    cancelled: CancellationToken,
    run_lock: tokio::sync::Mutex<()>,
}

fn baseline(draft: &MetadataDraft, state: &super::editor::Editing) -> MetadataDraft {
    let mut draft = draft.clone();
    for field in &mut draft.fields {
        if let Some(value) = state.values.get(&field.key) {
            field.value = value.clone();
        }
        field.mixed = state.mixed.contains(&field.key);
        field.suggestion = None;
    }
    draft
}

#[cfg_attr(feature = "bindings", uniffi::export)]
impl MetadataLookupReview {
    #[cfg_attr(feature = "bindings", uniffi::constructor)]
    pub async fn open(editor: Arc<MetadataEditor>) -> Result<Arc<Self>, Error> {
        editor.stage_changes().await?;
        if let Some(scope) = &editor.batch {
            return Self::open_batch(
                scope.batch.clone(),
                scope.indices.iter().map(|i| *i as u64).collect(),
            )
            .await;
        }
        let mut state = editor.state().clone();
        let draft = baseline(&editor.draft, &state);
        state.begin_lookup();
        let working = Arc::new(MetadataEditor {
            draft: Arc::new(draft),
            source: editor.source.clone(),
            session: editor.session.clone(),
            batch: None,
            external_lookup_allowed: editor.external_lookup_allowed,
            state: Mutex::new(state),
        });
        Ok(Self::new(
            Items::Single {
                original: editor,
                working,
            },
            1,
        ))
    }

    #[cfg_attr(feature = "bindings", uniffi::constructor)]
    pub async fn open_batch(
        batch: Arc<MetadataBatch>,
        indices: Vec<u64>,
    ) -> Result<Arc<Self>, Error> {
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
    }

    pub fn progress(&self) -> MetadataLookupProgress {
        self.progress
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    pub async fn next_update(&self) -> Option<MetadataLookupProgress> {
        self.updates.1.recv().await.ok().map(|_| self.progress())
    }

    pub fn cancel(&self) {
        self.cancelled.cancel();
    }

    pub async fn run(self: Arc<Self>) {
        let _run = self.run_lock.lock().await;
        let count = self.progress().total;
        for index in 0..count {
            let source = match &self.items {
                Items::Batch(batch) => &batch.source,
                Items::Single { working, .. } => &working.source,
            };
            if !source
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
                result = self.discover(index, String::new(), report) => result,
            };
            let mut progress = self.progress.lock().unwrap_or_else(|e| e.into_inner());
            progress.completed = index + 1;
            if let Err(error) = outcome {
                progress.failure = Some(error.to_string());
                break;
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
    }

    pub async fn tracks(
        &self,
        offset: u64,
        limit: u64,
    ) -> Result<Vec<MetadataTrackSummary>, Error> {
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
    }

    pub async fn item(&self, index: u64) -> Result<MetadataLookupItem, Error> {
        let editor = self.editor(index, false).await?;
        let mut state = editor.state().clone();
        state.apply_lookup_choice(&editor.draft, false);
        let fields = state.fields(&editor.draft);
        Ok(MetadataLookupItem {
            title: title_values(&state),
            ready: state.lookup_candidates.is_some(),
            candidates: state.lookup_candidates.clone().unwrap_or_default(),
            selected: state
                .lookup_preview
                .as_ref()
                .map(|candidate| candidate.id.clone()),
            fields: fields
                .into_iter()
                .map(|field| {
                    let original = editor
                        .draft
                        .fields
                        .iter()
                        .find(|original| original.key == field.field.key)
                        .map(|field| field.value.clone())
                        .unwrap_or_default();
                    MetadataLookupField {
                        changed: original != field.field.value,
                        label: field.field.label,
                        original,
                        suggested: field.field.value,
                    }
                })
                .collect(),
            failure: state.scan_error.clone(),
        })
    }

    pub async fn choose(
        &self,
        index: u64,
        candidate: Option<MetadataCandidate>,
    ) -> Result<(), Error> {
        let editor = self.editor(index, false).await?;
        let changes = if let Some(candidate) = &candidate {
            match self.resolve_candidate(&editor, candidate.clone()).await {
                Ok(changes) => Some(changes),
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
            if let Some(changes) = changes {
                state.lookup_result = candidate.clone().map(|candidate| (candidate, changes));
            }
            state.lookup_preview = candidate;
            state.scan_error = None;
        }
        self.store(index, &editor).await
    }

    pub async fn search(&self, index: u64, query: String) -> Result<(), Error> {
        let result = self.discover(index, query, Arc::new(|_| {})).await;
        if let Err(error) = &result {
            let editor = self.editor(index, false).await?;
            editor.state().scan_error = Some(error.to_string());
            self.store(index, &editor).await?;
        }
        result
    }

    pub async fn accept(&self, fill_missing: bool) -> Result<(), Error> {
        self.cancel();
        let _run = self.run_lock.lock().await;
        match &self.items {
            Items::Batch(batch) => batch.accept_lookup(fill_missing).await,
            Items::Single { original, working } => {
                let mut state = working.state().clone();
                if state.lookup_preview.is_some() {
                    state.apply_lookup_choice(&working.draft, fill_missing);
                    *original.state() = state;
                }
                Ok(())
            }
        }
    }
}

impl MetadataLookupReview {
    fn new(items: Items, total: u64) -> Arc<Self> {
        Arc::new(Self {
            items,
            response: Mutex::new(None),
            progress: Mutex::new(MetadataLookupProgress {
                completed: 0,
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
        let _ = self.updates.0.try_send(());
        Ok(())
    }

    async fn discover(
        &self,
        index: u64,
        query: String,
        report: LookupProgress,
    ) -> Result<(), Error> {
        report(&tr(msgid("Reading tags...")));
        let mut editor = self.editor(index, false).await?;
        if query.is_empty() && editor.state().lookup_candidates.is_some() {
            return Ok(());
        }
        let mut candidates = Vec::new();
        let mut failure = None;
        if query.is_empty() && editor.draft.kind == SourceEntityKind::Track {
            match self.editor(index, true).await {
                Ok(scanning) => {
                    editor = scanning;
                    match editor.clone().scan(report.clone()).await {
                        Ok(found) => candidates = found,
                        Err(error) => failure = Some(error.to_string()),
                    }
                }
                Err(error) => failure = Some(error.to_string()),
            }
        }
        if candidates.is_empty() {
            report(&tr(msgid("Searching MusicBrainz...")));
            candidates = self.search_candidates(&editor, query).await?;
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
                    let recordings = state.scan.as_ref().map(|(_, matches)| &matches.recordings);
                    if candidates
                        .iter()
                        .any(|candidate| candidate.from_fingerprint)
                        && recordings.is_some_and(|recordings| recordings.len() == 1)
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
            match self.resolve_candidate(&editor, chosen.clone()).await {
                Ok(changes) => Some((chosen, changes)),
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
            state.lookup_preview = resolved.as_ref().map(|(candidate, _)| candidate.clone());
            if let Some(resolved) = resolved {
                state.lookup_result = Some(resolved);
            }
            state.scan_error = failure;
            state.lookup_candidates = Some(candidates);
        }
        self.store(index, &editor).await
    }
}

impl MetadataLookupReview {
    async fn search_candidates(
        &self,
        editor: &MetadataEditor,
        query: String,
    ) -> Result<Vec<MetadataCandidate>, Error> {
        if !editor
            .source
            .shared
            .settings
            .load()
            .ui
            .allows_external_metadata_lookup()
        {
            return Ok(Vec::new());
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
            return Ok(vec![candidate]);
        }
        editor
            .source
            .shared
            .runtime
            .spawn_blocking(move || metadata_lookup::search_metadata(kind, &values, &query))
            .await
            .map_err(|e| Error::Write(e.to_string()))?
            .map_err(Error::Write)
    }

    async fn resolve_candidate(
        &self,
        editor: &MetadataEditor,
        candidate: MetadataCandidate,
    ) -> Result<sources::MetadataChanges, Error> {
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
        {
            let state = editor.state();
            if let Some((cached, changes)) = &state.lookup_result
                && cached == &candidate
            {
                return Ok(changes.clone());
            }
        }
        let cached = self
            .response
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .as_ref()
            .filter(|(known, _)| {
                known.id == candidate.id
                    && known.kind == candidate.kind
                    && known.from_fingerprint == candidate.from_fingerprint
            })
            .map(|(_, response)| response.clone());
        let lookup = if let Some(response) = cached {
            response
        } else {
            let request = candidate.clone();
            let response = Arc::new(
                editor
                    .source
                    .shared
                    .runtime
                    .spawn_blocking(move || metadata_lookup::lookup_metadata(&request))
                    .await
                    .map_err(|e| Error::Write(e.to_string()))?
                    .map_err(Error::Write)?,
            );
            self.response
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .replace((candidate, response.clone()));
            response
        };
        let state = editor.state();
        lookup
            .changes(
                editor.draft.kind,
                &state.values(),
                state
                    .scan
                    .as_ref()
                    .map_or(&[], |(_, matches)| matches.recordings.as_slice()),
            )
            .ok_or_else(|| Error::InvalidInput(tr(msgid("No matching track in this release"))))
    }
}

fn title_values(state: &super::editor::Editing) -> String {
    state.values.get("title").cloned().unwrap_or_default()
}

pub(super) fn lookup_status(state: &super::editor::Editing) -> String {
    match &state.lookup_candidates {
        None => String::new(),
        Some(_) if state.lookup_preview.is_some() => localization::tr("Matched"),
        Some(candidates) if candidates.is_empty() => localization::tr("No results"),
        Some(_) => localization::tr("Needs review"),
    }
}
