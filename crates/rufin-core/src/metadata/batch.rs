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
    draft: MetadataDraft,
    pub(super) state: Editing,
    failure: Option<String>,
}

pub(super) struct Records {
    file: File,
    offsets: Vec<(u64, u64)>,
    artwork: HashMap<usize, ArtworkChange>,
    changed: HashSet<usize>,
}

impl Records {
    pub(super) fn read(&mut self, index: usize) -> Result<Record, Error> {
        let (offset, length) = self.offsets[index];
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
        if index == self.offsets.len() {
            self.offsets.push((offset, bytes.len() as u64));
        } else {
            self.offsets[index] = (offset, bytes.len() as u64);
        }
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
    source: Arc<SourceOwner>,
    uris: Vec<String>,
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
            .filter(|uri| seen.insert(uri.clone()))
            .collect::<Vec<_>>();
        drop(seen);
        if uris.is_empty() {
            return Err(Error::Unavailable);
        }
        let batch = Arc::new(Self {
            source,
            uris,
            records: Mutex::new(Records {
                file: tempfile::tempfile().map_err(error)?,
                offsets: Vec::new(),
                artwork: HashMap::new(),
                changed: HashSet::new(),
            }),
            changed: AtomicBool::new(false),
        });
        for (index, uri) in batch.uris.iter().enumerate() {
            let (draft, failure) =
                match super::EditSession::open(&batch.source, &MetadataItemId::Track(uri.clone()))
                    .await
                {
                    Ok((_, draft)) => (draft, None),
                    Err(error @ Error::LocalAccessRequired { .. }) => return Err(error),
                    Err(error) => (
                        MetadataDraft::new(SourceEntityKind::Track, Vec::new()),
                        Some(error.to_string()),
                    ),
                };
            let record = Record {
                state: Editing::new(&draft),
                draft,
                failure,
            };
            if record.state.has_changes() {
                batch.changed.store(true, Ordering::Relaxed);
            }
            batch
                .work(move |records| records.write(index, &record))
                .await?;
        }
        Ok(batch)
    }

    pub fn track_count(&self) -> u64 {
        self.uris.len() as u64
    }
    pub fn has_changes(&self) -> bool {
        self.changed.load(Ordering::Relaxed)
    }

    pub async fn tracks(
        self: Arc<Self>,
        offset: u64,
        limit: u64,
    ) -> Result<Vec<MetadataTrackSummary>, Error> {
        let end = self.uris.len().min(offset.saturating_add(limit) as usize);
        let mut summaries: Vec<MetadataTrackSummary> = self
            .work(move |records| {
                (offset as usize..end)
                    .map(|index| {
                        let record = records.read(index)?;
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
                            scan_status: record.state.scan.as_ref().map_or_else(
                                String::new,
                                |(_, matches)| {
                                    if matches.candidates.is_empty() {
                                        localization::tr("No results")
                                    } else {
                                        localization::trn_with(
                                            "{count} result",
                                            "{count} results",
                                            matches.candidates.len() as u64,
                                            &[("count", &matches.candidates.len().to_string())],
                                        )
                                    }
                                },
                            ),
                            error: record.failure.or(record.state.scan_error),
                        })
                    })
                    .collect()
            })
            .await?;
        let missing = summaries
            .iter()
            .filter(|row: &&MetadataTrackSummary| row.title.is_empty())
            .map(|row| self.uris[row.index as usize].clone())
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
                    let uri = &self.uris[row.index as usize];
                    row.title = titles.get(uri).cloned().unwrap_or_else(|| uri.clone());
                }
            }
        }
        Ok(summaries)
    }

    pub async fn editor(self: Arc<Self>, indices: Vec<u64>) -> Result<Arc<MetadataEditor>, Error> {
        let indices = if indices.is_empty() {
            (0..self.uris.len()).collect::<Vec<_>>()
        } else {
            indices.into_iter().map(|index| index as usize).collect()
        };
        if indices.iter().any(|index| *index >= self.uris.len()) {
            return Err(Error::Unavailable);
        }
        let selected = indices.clone();
        let selected_len = selected.len();
        let (draft, current, identified, artwork, has_artwork, lyrics, scan, candidates, preview) =
            self.work(move |records| {
                let mut identified = HashSet::new();
                let mut has_artwork = false;
                let mut lyrics = None;
                let mut scan = None;
                let mut candidates = Vec::new();
                let mut preview = None;
                let mut artwork: Option<Option<ArtworkChange>> = None;
                let mut combined: Option<MetadataDraft> = None;
                let mut originals: Option<MetadataDraft> = None;
                for index in selected {
                    let record = records.read(index)?;
                    super::scan::merge_candidates(
                        &mut candidates,
                        record.state.scan_candidates(),
                        selected_len,
                    );
                    match &mut preview {
                        None => preview = Some(record.state.lookup_preview.clone()),
                        Some(current)
                            if current
                                .as_ref()
                                .map(|candidate: &super::MetadataCandidate| &candidate.id)
                                != record
                                    .state
                                    .lookup_preview
                                    .as_ref()
                                    .map(|candidate| &candidate.id) =>
                        {
                            *current = None
                        }
                        _ => {}
                    }
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
                    candidates,
                    preview.flatten(),
                ))
            })
            .await?;
        let mut draft = draft;
        draft.track_count = indices.len();
        let mut state = Editing::new(&current);
        state.artwork = artwork.clone();
        state.lyrics = lyrics;
        state.scan = scan;
        state.batch_candidates = candidates;
        state.lookup_preview = preview;
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
            lookup: Mutex::new(None),
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
        for index in 0..self.uris.len() {
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
                    &MetadataItemId::Track(self.uris[index].clone()),
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
                    &MetadataItemId::Track(self.uris[index].clone()),
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
                    &MetadataItemId::Track(self.uris[index].clone()),
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

    pub(super) async fn preview_lookup(
        self: &Arc<Self>,
        indices: &[usize],
        lookup: Option<Arc<metadata_lookup::MetadataLookup>>,
        candidate: Option<super::MetadataCandidate>,
        fill_missing: bool,
    ) -> Result<(), Error> {
        let indices = indices.to_vec();
        let (unmatched, changed) = self
            .work(move |records| {
                let mut unmatched = Vec::new();
                for index in indices {
                    let mut record = records.read(index)?;
                    record.state.clear_preview();
                    if let Some(lookup) = &lookup {
                        if let Some(changes) = lookup.changes(
                            SourceEntityKind::Track,
                            &record.state.values(),
                            record
                                .state
                                .scan
                                .as_ref()
                                .map_or(&[], |(_, matches)| matches.recordings.as_slice()),
                        ) {
                            record
                                .state
                                .preview_identified(&record.draft, changes, fill_missing);
                            record.state.lookup_preview = candidate.clone();
                        } else {
                            unmatched.push(
                                record
                                    .state
                                    .values
                                    .get("title")
                                    .cloned()
                                    .unwrap_or_else(|| (index + 1).to_string()),
                            );
                        }
                    }
                    records.write(index, &record)?;
                }
                Ok((unmatched, !records.changed.is_empty()))
            })
            .await?;
        self.changed.store(changed, Ordering::Relaxed);
        if unmatched.is_empty() {
            Ok(())
        } else {
            Err(Error::Write(localization::tr_with(
                "No matching track in this release: {tracks}",
                &[("tracks", unmatched.join("; ").as_str())],
            )))
        }
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
            &MetadataItemId::Track(self.uris[index].clone()),
        )
        .await
        .map(|(session, _)| session)
    }

    pub(super) async fn read(self: &Arc<Self>, index: usize) -> Result<Record, Error> {
        self.work(move |records| records.read(index)).await
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
        let changed = self
            .work(move |records| {
                for index in indices {
                    let mut record = records.read(index)?;
                    for (key, value) in &edit.changes {
                        if record.state.values.get(key) != Some(value) {
                            record.state.set_text(key, value.clone());
                        }
                    }
                    if let Some(artwork) = &edit.artwork {
                        record.state.artwork = Some(artwork.change.clone());
                        record.state.storage =
                            if record.draft.artwork.storage == sources::ArtworkStorage::Server {
                                sources::ArtworkStorage::Server
                            } else {
                                artwork.storage
                            };
                    }
                    if lyrics.is_some() {
                        record.state.lyrics = lyrics.clone();
                    }
                    records.write(index, &record)?;
                }
                Ok(!records.changed.is_empty())
            })
            .await?;
        let mut state = editor.state();
        state.touched.clear();
        state.artwork = None;
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
