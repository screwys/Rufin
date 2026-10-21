use super::editor::Editing;
use super::{MetadataEditor, MetadataItemId};
use crate::source::SourceOwner;
use sources::{ArtworkChange, MetadataDraft, SourceEntityKind, SourceMetadataError as Error};
use std::{
    collections::{HashMap, HashSet},
    fs::File,
    io::{Read, Seek, SeekFrom, Write},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
};

fn error(error: impl std::fmt::Display) -> Error {
    Error::Write(error.to_string())
}

#[derive(serde::Serialize, serde::Deserialize)]
pub(super) struct Record {
    pub(super) draft: MetadataDraft,
    pub(super) state: Editing,
    failure: Option<String>,
}

pub(super) struct Records {
    file: File,
    offsets: HashMap<usize, (u64, u64)>,
    artwork: HashMap<usize, ArtworkChange>,
    changed: HashSet<usize>,
}

impl Records {
    pub(super) fn read(&mut self, index: usize) -> Result<Record, Error> {
        let (offset, length) = *self.offsets.get(&index).ok_or(Error::Unavailable)?;
        self.file.seek(SeekFrom::Start(offset)).map_err(error)?;
        let mut bytes = vec![0; length as usize];
        self.file.read_exact(&mut bytes).map_err(error)?;
        let mut record: Record = serde_json::from_slice(&bytes).map_err(error)?;
        record.state.artwork = self.artwork.get(&index).cloned();
        Ok(record)
    }

    pub(super) fn write(&mut self, index: usize, record: &Record) -> Result<(), Error> {
        if record.state.has_changes() || record.state.artwork.is_some() {
            self.changed.insert(index);
        } else {
            self.changed.remove(&index);
        }
        let bytes = serde_json::to_vec(record).map_err(error)?;
        let offset = self.file.seek(SeekFrom::End(0)).map_err(error)?;
        self.file.write_all(&bytes).map_err(error)?;
        self.offsets.insert(index, (offset, bytes.len() as u64));
        if let Some(artwork) = &record.state.artwork {
            self.artwork.insert(index, artwork.clone());
        } else {
            self.artwork.remove(&index);
        }
        Ok(())
    }
}

#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
#[derive(Clone, PartialEq, Eq)]
pub struct MetadataTrackSummary {
    pub index: u64,
    pub title: String,
    pub modified: bool,
    pub error: Option<String>,
    pub scan_status: String,
}

pub(super) struct BatchScope {
    pub batch: Arc<MetadataBatch>,
    pub indices: Vec<usize>,
    pub has_artwork: bool,
    artwork: Option<ArtworkChange>,
    storage: sources::ArtworkStorage,
}

#[cfg_attr(feature = "bindings", derive(uniffi::Object))]
pub struct MetadataBatch {
    pub(super) source: Arc<SourceOwner>,
    parent: Option<(Arc<Self>, Vec<usize>)>,
    uris: Vec<Arc<str>>,
    // Original tags and pending drafts stay off the UI heap for large selections.
    records: Mutex<Records>,
    changed: AtomicBool,
}

#[cfg_attr(feature = "bindings", uniffi::export)]
impl MetadataBatch {
    #[cfg_attr(feature = "bindings", uniffi::constructor)]
    pub async fn open(
        source: Arc<SourceOwner>,
        selection: Arc<library::MemberSelection>,
    ) -> Result<Arc<Self>, Error> {
        let mut seen = HashSet::new();
        let uris = source
            .shared
            .database
            .selected_track_uris(&selection, None, &library::ReadCancellation::new())
            .await
            .map_err(error)?
            .into_iter()
            .map(Arc::<str>::from)
            .filter(|uri| seen.insert(uri.clone()))
            .collect::<Vec<_>>();
        drop(seen);
        if uris.is_empty() {
            return Err(Error::Unavailable);
        }
        let batch = Arc::new(Self {
            source,
            parent: None,
            uris,
            records: Mutex::new(Records {
                file: tempfile::tempfile().map_err(error)?,
                offsets: HashMap::new(),
                artwork: HashMap::new(),
                changed: HashSet::new(),
            }),
            changed: AtomicBool::new(false),
        });
        Ok(batch)
    }

    pub fn external_lookup_allowed(&self) -> bool {
        self.source
            .shared
            .settings
            .load()
            .ui
            .allows_external_metadata_lookup()
    }

    pub fn track_count(&self) -> u64 {
        self.len() as u64
    }
    pub fn has_changes(&self) -> bool {
        self.changed.load(Ordering::Relaxed)
    }

    pub async fn tracks(
        self: Arc<Self>,
        offset: u64,
        limit: u64,
    ) -> Result<Vec<MetadataTrackSummary>, Error> {
        let end = self.len().min(offset.saturating_add(limit) as usize);
        let mut summaries: Vec<MetadataTrackSummary> = self
            .work(move |records| {
                (offset as usize..end)
                    .map(|index| {
                        let record = if records.offsets.contains_key(&index) {
                            Some(records.read(index)?)
                        } else {
                            None
                        };
                        let Some(record) = record else {
                            return Ok(MetadataTrackSummary {
                                index: index as u64,
                                title: String::new(),
                                modified: false,
                                error: None,
                                scan_status: String::new(),
                            });
                        };
                        Ok(MetadataTrackSummary {
                            index: index as u64,
                            title: record
                                .state
                                .values
                                .get("title")
                                .filter(|title| !title.is_empty())
                                .cloned()
                                .unwrap_or_default(),
                            modified: record.state.has_changes() || record.state.artwork.is_some(),
                            scan_status: super::review::lookup_status(&record.state),
                            error: record.failure.or(record.state.scan_error),
                        })
                    })
                    .collect()
            })
            .await?;
        let missing = summaries
            .iter()
            .filter(|row: &&MetadataTrackSummary| row.title.is_empty())
            .map(|row| self.uri(row.index as usize).to_string())
            .collect::<Vec<_>>();
        if !missing.is_empty() {
            let titles = self
                .source
                .shared
                .database
                .tracks_by_uris(&missing, &library::ReadCancellation::new())
                .await
                .map_err(error)?
                .into_iter()
                .map(|row| (row.media_uri, row.metadata.title))
                .collect::<HashMap<_, _>>();
            for row in &mut summaries {
                if row.title.is_empty() {
                    let uri = self.uri(row.index as usize);
                    row.title = titles.get(uri).cloned().unwrap_or_else(|| uri.to_string());
                }
            }
        }
        Ok(summaries)
    }

    pub async fn editor(self: Arc<Self>, indices: Vec<u64>) -> Result<Arc<MetadataEditor>, Error> {
        let indices = if indices.is_empty() {
            (0..self.len()).collect::<Vec<_>>()
        } else {
            indices
                .into_iter()
                .map(|index| usize::try_from(index).map_err(|_| Error::Unavailable))
                .collect::<Result<Vec<_>, _>>()?
        };
        if indices.iter().any(|index| *index >= self.len()) {
            return Err(Error::Unavailable);
        }
        for &index in &indices {
            self.read(index).await?;
        }
        let cancelled = tokio_util::sync::CancellationToken::new();
        let _cancel = cancelled.clone().drop_guard();
        let selected = indices.clone();
        let selected_len = selected.len();
        let (draft, current, identified, artwork, has_artwork, lyrics, scan) = self
            .work(move |records| {
                let mut identified = HashSet::new();
                let mut has_artwork = false;
                let mut lyrics = None;
                let mut scan = None;
                let mut artwork: Option<Option<ArtworkChange>> = None;
                let mut combined: Option<MetadataDraft> = None;
                let mut originals: Option<MetadataDraft> = None;
                for index in selected {
                    if cancelled.is_cancelled() {
                        return Err(Error::Unavailable);
                    }
                    let record = records.read(index)?;
                    if selected_len == 1 {
                        lyrics = record.state.lyrics.clone();
                        scan = record.state.scan.clone();
                    }
                    has_artwork |= match &record.state.artwork {
                        Some(ArtworkChange::Replace(_)) => true,
                        Some(ArtworkChange::Remove) => false,
                        None => record.draft.artwork.binding.is_some(),
                    };
                    identified.extend(
                        record
                            .state
                            .identified
                            .keys()
                            .filter(|key| record.state.is_identified(key))
                            .cloned(),
                    );
                    identified.extend(record.state.touched.iter().cloned());
                    match &artwork {
                        None => artwork = Some(record.state.artwork.clone()),
                        Some(current) if current != &record.state.artwork => artwork = Some(None),
                        _ => {}
                    }
                    if let Some(original) = &mut originals {
                        original.merge(record.draft.clone());
                    } else {
                        originals = Some(record.draft.clone());
                    }
                    let mut draft = record.draft;
                    draft.artwork.storage = record.state.storage;
                    for field in &mut draft.fields {
                        if let Some(value) = record.state.values.get(&field.key) {
                            field.value = value.clone();
                        }
                        field.suggestion = None;
                    }
                    if let Some(current) = &mut combined {
                        current.merge(draft);
                    } else {
                        combined = Some(draft);
                    }
                }
                let draft = combined.ok_or(Error::Unavailable)?;
                Ok((
                    originals.unwrap(),
                    draft,
                    identified,
                    artwork.flatten(),
                    has_artwork,
                    lyrics,
                    scan,
                ))
            })
            .await?;
        let mut draft = draft;
        draft.track_count = indices.len();
        let mut state = Editing::new(&current);
        state.artwork = artwork.clone();
        state.lyrics = lyrics;
        state.scan = scan;
        let storage = state.storage;
        for key in identified {
            state.identified.insert(
                key.clone(),
                super::editor::IdentifiedField {
                    original: draft
                        .fields
                        .iter()
                        .find(|field| field.key == key)
                        .map(|field| field.value.clone())
                        .unwrap_or_default(),
                    original_mixed: draft
                        .fields
                        .iter()
                        .any(|field| field.key == key && field.mixed),
                    touched: false,
                },
            );
        }
        Ok(Arc::new(MetadataEditor {
            draft: Arc::new(draft),
            source: self.source.clone(),
            session: None,
            external_lookup_allowed: self
                .source
                .shared
                .settings
                .load()
                .ui
                .allows_external_metadata_lookup(),
            state: Mutex::new(state),
            batch: Some(BatchScope {
                batch: self.clone(),
                indices,
                has_artwork,
                artwork,
                storage,
            }),
        }))
    }

    pub async fn save(self: Arc<Self>) -> Result<(), Error> {
        let mut failures = Vec::new();
        let changed_indices = self
            .work(|records| Ok(records.changed.iter().copied().collect::<Vec<_>>()))
            .await?;
        for index in changed_indices {
            let mut record = self.read(index).await?;
            if !record.state.has_changes() && record.state.artwork.is_none() {
                continue;
            }
            let result = async {
                let edit = record
                    .state
                    .edit(&record.draft)
                    .map_err(Error::InvalidInput)?;
                let (session, current) = super::EditSession::open(
                    &self.source,
                    &MetadataItemId::Track(self.uri(index).to_string()),
                )
                .await?;
                if current.revision != record.draft.revision {
                    return Err(Error::Conflict);
                }
                session
                    .save(&self.source, edit, record.state.lyrics.clone())
                    .await
            }
            .await;
            let saved = matches!(result, Ok(()) | Err(Error::SavedRefreshFailed(_)));
            if saved {
                match super::EditSession::open(
                    &self.source,
                    &MetadataItemId::Track(self.uri(index).to_string()),
                )
                .await
                {
                    Ok((_, draft)) => {
                        record.state = Editing::new(&draft);
                        record.draft = draft;
                    }
                    Err(_) => {
                        record.state.touched.clear();
                        record.state.artwork = None;
                        record.state.lyrics = None;
                    }
                }
            }
            if matches!(result, Err(Error::PartiallySaved { .. })) {
                if let Ok((_, current)) = super::EditSession::open(
                    &self.source,
                    &MetadataItemId::Track(self.uri(index).to_string()),
                )
                .await
                {
                    record.draft.revision = current.revision;
                }
            }
            record.failure = result.as_ref().err().map(ToString::to_string);
            if let Err(error) = result {
                failures.push(format!(
                    "{}: {error}",
                    record
                        .state
                        .values
                        .get("title")
                        .cloned()
                        .unwrap_or_else(|| (index + 1).to_string())
                ));
            }
            self.work(move |records| records.write(index, &record))
                .await?;
        }
        let changed = self.work(|records| Ok(!records.changed.is_empty())).await?;
        self.changed.store(changed, Ordering::Relaxed);
        if failures.is_empty() {
            Ok(())
        } else {
            Err(Error::Write(failures.join("\n")))
        }
    }
}

impl MetadataBatch {
    pub(super) async fn work<T: Send + 'static>(
        self: &Arc<Self>,
        work: impl FnOnce(&mut Records) -> Result<T, Error> + Send + 'static,
    ) -> Result<T, Error> {
        let batch = self.clone();
        self.source
            .shared
            .runtime
            .spawn_blocking(move || {
                work(&mut batch.records.lock().unwrap_or_else(|e| e.into_inner()))
            })
            .await
            .map_err(error)?
    }

    pub(super) async fn reset(
        self: &Arc<Self>,
        indices: &[usize],
        key: String,
    ) -> Result<(), Error> {
        let indices = indices.to_vec();
        let changed = self
            .work(move |records| {
                for index in indices {
                    let mut record = records.read(index)?;
                    if !record.state.undo_identified(&key) {
                        if let Some(field) =
                            record.draft.fields.iter().find(|field| field.key == key)
                        {
                            record.state.values.insert(key.clone(), field.value.clone());
                            record.state.touched.remove(&key);
                        }
                    }
                    records.write(index, &record)?;
                }
                Ok(!records.changed.is_empty())
            })
            .await?;
        self.changed.store(changed, Ordering::Relaxed);
        Ok(())
    }

    pub(super) async fn session(&self, index: usize) -> Result<Arc<super::EditSession>, Error> {
        super::EditSession::open(
            &self.source,
            &MetadataItemId::Track(self.uri(index).to_string()),
        )
        .await
        .map(|(session, _)| session)
    }

    fn len(&self) -> usize {
        self.parent
            .as_ref()
            .map_or(self.uris.len(), |(_, indices)| indices.len())
    }

    fn uri(&self, index: usize) -> &str {
        match &self.parent {
            Some((parent, indices)) => parent.uri(indices[index]),
            None => &self.uris[index],
        }
    }

    pub(super) fn fork(self: &Arc<Self>, indices: Vec<usize>) -> Result<Arc<Self>, Error> {
        Ok(Arc::new(Self {
            source: self.source.clone(),
            parent: Some((self.clone(), indices)),
            uris: Vec::new(),
            records: Mutex::new(Records {
                file: tempfile::tempfile().map_err(error)?,
                offsets: HashMap::new(),
                artwork: HashMap::new(),
                changed: HashSet::new(),
            }),
            changed: AtomicBool::new(false),
        }))
    }

    pub(super) async fn read(self: &Arc<Self>, index: usize) -> Result<Record, Error> {
        if index >= self.len() {
            return Err(Error::Unavailable);
        }
        if self
            .work(move |records| Ok(records.offsets.contains_key(&index)))
            .await?
        {
            return self.work(move |records| records.read(index)).await;
        }
        let record = if let Some((parent, indices)) = &self.parent {
            let mut record = Box::pin(parent.read(indices[index])).await?;
            for field in &mut record.draft.fields {
                if let Some(value) = record.state.values.get(&field.key) {
                    field.value = value.clone();
                }
                field.mixed = record.state.mixed.contains(&field.key);
                field.suggestion = None;
            }
            record.state.begin_lookup();
            record
        } else {
            let (draft, failure) = match super::EditSession::open(
                &self.source,
                &MetadataItemId::Track(self.uri(index).to_string()),
            )
            .await
            {
                Ok((_, draft)) => (draft, None),
                Err(error @ Error::LocalAccessRequired { .. }) => return Err(error),
                Err(error) => (
                    MetadataDraft::new(SourceEntityKind::Track, Vec::new()),
                    Some(error.to_string()),
                ),
            };
            Record {
                state: Editing::new(&draft),
                draft,
                failure,
            }
        };
        let changed = self
            .work(move |records| {
                if !records.offsets.contains_key(&index) {
                    records.write(index, &record)?;
                }
                Ok(!records.changed.is_empty())
            })
            .await?;
        self.changed.store(changed, Ordering::Relaxed);
        self.work(move |records| records.read(index)).await
    }

    pub(super) async fn detached_editor(
        self: &Arc<Self>,
        index: usize,
        scan: bool,
    ) -> Result<Arc<MetadataEditor>, Error> {
        let record = self.read(index).await?;
        Ok(Arc::new(MetadataEditor {
            draft: Arc::new(record.draft),
            source: self.source.clone(),
            session: if scan {
                Some(self.session(index).await?)
            } else {
                None
            },
            batch: None,
            external_lookup_allowed: self
                .source
                .shared
                .settings
                .load()
                .ui
                .allows_external_metadata_lookup(),
            state: Mutex::new(record.state),
        }))
    }

    pub(super) async fn store_editor(
        self: &Arc<Self>,
        index: usize,
        editor: &MetadataEditor,
    ) -> Result<(), Error> {
        let state = editor.state().clone();
        self.work(move |records| {
            let mut record = records.read(index)?;
            record.state = state;
            records.write(index, &record)
        })
        .await
    }

    pub(super) async fn accept_lookup(self: &Arc<Self>, fill_missing: bool) -> Result<(), Error> {
        let Some((parent, indices)) = &self.parent else {
            return Ok(());
        };
        let loaded = self
            .work(|records| Ok(records.offsets.keys().copied().collect::<Vec<_>>()))
            .await?;
        for index in loaded {
            let mut reviewed = self.read(index).await?;
            let chosen = reviewed.state.lookup_preview.is_some();
            if !chosen && reviewed.state.lookup_candidates.is_none() {
                continue;
            }
            if chosen {
                reviewed
                    .state
                    .apply_lookup_choice(&reviewed.draft, fill_missing);
            }
            let target = indices[index];
            parent
                .work(move |records| {
                    let mut original = records.read(target)?;
                    if chosen {
                        original.state = reviewed.state;
                    } else {
                        original.state.scan = reviewed.state.scan;
                        original.state.lookup_candidates = reviewed.state.lookup_candidates;
                        original.state.scan_error = reviewed.state.scan_error;
                    }
                    records.write(target, &original)
                })
                .await?;
        }
        let changed = parent
            .work(|records| Ok(!records.changed.is_empty()))
            .await?;
        parent.changed.store(changed, Ordering::Relaxed);
        Ok(())
    }

    pub(super) async fn stage(self: &Arc<Self>, editor: &MetadataEditor) -> Result<(), Error> {
        let scope = editor.batch.as_ref().unwrap();
        let mut edit = editor
            .state()
            .edit(&editor.draft)
            .map_err(Error::InvalidInput)?;
        if edit.artwork.as_ref().is_some_and(|artwork| {
            scope.artwork.as_ref() == Some(&artwork.change) && scope.storage == artwork.storage
        }) {
            edit.artwork = None;
        }
        let lyrics = editor.state().lyrics.clone();
        if edit.changes.is_empty() && edit.artwork.is_none() && lyrics.is_none() {
            return Ok(());
        }
        let indices = scope.indices.clone();
        let cancelled = tokio_util::sync::CancellationToken::new();
        let _cancel = cancelled.clone().drop_guard();
        let changed = self
            .work(move |records| {
                for index in indices {
                    if cancelled.is_cancelled() {
                        return Err(Error::Unavailable);
                    }
                    let mut record = records.read(index)?;
                    let mut changed = lyrics.is_some();
                    for (key, value) in &edit.changes {
                        if record.state.values.get(key) != Some(value) {
                            changed = true;
                            record.state.set_text(key, value.clone());
                        }
                    }
                    if let Some(artwork) = &edit.artwork {
                        let storage =
                            if record.draft.artwork.storage == sources::ArtworkStorage::Server {
                                sources::ArtworkStorage::Server
                            } else {
                                artwork.storage
                            };
                        if record.state.artwork.as_ref() != Some(&artwork.change)
                            || record.state.storage != storage
                        {
                            record.state.artwork = Some(artwork.change.clone());
                            record.state.storage = storage;
                            changed = true;
                        }
                    }
                    if lyrics.is_some() {
                        record.state.lyrics = lyrics.clone();
                    }
                    if changed {
                        records.write(index, &record)?;
                    }
                }
                Ok(!records.changed.is_empty())
            })
            .await?;
        let mut state = editor.state();
        for key in std::mem::take(&mut state.touched) {
            if let Some(field) = editor.draft.fields.iter().find(|field| field.key == key) {
                state
                    .identified
                    .entry(key)
                    .or_insert_with(|| super::editor::IdentifiedField {
                        original: field.value.clone(),
                        original_mixed: field.mixed,
                        touched: false,
                    });
            }
        }
        self.changed.store(changed, Ordering::Relaxed);
        Ok(())
    }
}

impl MetadataEditor {
    pub(super) async fn reload_batch(&self) -> Result<(), Error> {
        if let Some(scope) = &self.batch {
            let editor = scope
                .batch
                .clone()
                .editor(scope.indices.iter().map(|index| *index as u64).collect())
                .await?;
            let mut next = editor.state().clone();
            let mut state = self.state();
            next.current = state.current.clone();
            next.artwork_revision = state.artwork_revision.wrapping_add(1);
            *state = next;
        }
        Ok(())
    }
}
