use super::editor::Editing;
use super::{MetadataEditor, MetadataItemId};
use crate::source::SourceOwner;
use sources::{ArtworkChange, MetadataDraft, SourceEntityKind, SourceMetadataError as Error};
use std::{
    collections::{HashMap, HashSet},
    fs::File,
    io::{Read, Seek, SeekFrom, Write},
    sync::{Arc, Mutex},
};

fn error(error: impl std::fmt::Display) -> Error {
    Error::Write(error.to_string())
}

#[derive(Clone, serde::Serialize, serde::Deserialize)]
pub(super) struct Record {
    pub(super) draft: MetadataDraft,
    pub(super) state: Editing,
    failure: Option<String>,
    scope_artwork: bool,
    scope_artist: Option<String>,
}

pub(super) struct Records {
    file: File,
    scope: Option<Record>,
    offsets: HashMap<usize, (u64, u64)>,
    artwork: HashMap<usize, ArtworkChange>,
    pub(super) changed: HashSet<usize>,
}

impl Records {
    fn new() -> Result<Self, Error> {
        Ok(Self {
            file: tempfile::tempfile().map_err(error)?,
            scope: None,
            offsets: HashMap::new(),
            artwork: HashMap::new(),
            changed: HashSet::new(),
        })
    }

    fn changed_in<'a>(&'a self, indices: &'a [usize]) -> Box<dyn Iterator<Item = usize> + 'a> {
        if indices.len() < self.changed.len() {
            Box::new(
                indices
                    .iter()
                    .copied()
                    .filter(|index| self.changed.contains(index)),
            )
        } else {
            Box::new(
                self.changed
                    .iter()
                    .copied()
                    .filter(|index| indices.binary_search(index).is_ok()),
            )
        }
    }

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
        if record.state.has_changes(&record.draft)
            || record.state.artwork.is_some()
            || record.scope_artwork
        {
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
    // Sorted so changed-track reads can check membership without another set.
    pub indices: Vec<usize>,
    pub has_artwork: bool,
    artwork: Option<ArtworkChange>,
    storage: sources::ArtworkStorage,
}

impl BatchScope {
    pub(super) fn changed_count(&self) -> usize {
        let records = self
            .batch
            .records
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        if self.indices.len() == self.batch.len() {
            records.changed.len()
        } else {
            records.changed_in(&self.indices).count()
        }
    }
}

struct FileScope {
    session: Arc<super::EditSession>,
    title: String,
}

#[cfg_attr(feature = "bindings", derive(uniffi::Object))]
pub struct MetadataBatch {
    scope: Option<Arc<FileScope>>,
    pub(super) source: Arc<SourceOwner>,
    parent: Option<(Arc<Self>, Vec<usize>)>,
    uris: Vec<Arc<str>>,
    // Original tags and pending drafts stay off the UI heap for large selections.
    records: Mutex<Records>,
}

#[cfg_attr(feature = "bindings", uniffi::export)]
impl MetadataBatch {
    #[cfg_attr(feature = "bindings", uniffi::constructor)]
    pub async fn open(
        source: Arc<SourceOwner>,
        selection: Arc<library::MemberSelection>,
    ) -> Result<Arc<Self>, Error> {
        let runtime = source.shared.runtime.clone();
        super::on_runtime(runtime, async {
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
            Self::from_uris(source, uris)
        })
        .await
    }

    pub fn scope_title(&self) -> Option<String> {
        self.scope.as_ref().map(|scope| scope.title.clone())
    }

    pub async fn scope_editor(self: Arc<Self>) -> Result<Arc<MetadataEditor>, Error> {
        let runtime = self.source.shared.runtime.clone();
        super::on_runtime(runtime, async {
            let length = self.len();
            let (record, mut state) = self
                .work(move |records| {
                    let record = records.scope.clone().ok_or(Error::Unavailable)?;
                    let artist = record
                        .draft
                        .fields
                        .iter()
                        .find(|field| field.key == "title")
                        .map(|field| field.value.as_str())
                        .unwrap_or_default();
                    let mut combined =
                        (records.offsets.len() < length).then(|| record.draft.clone());
                    let mut changed = HashSet::new();
                    let indices = records.offsets.keys().copied().collect::<Vec<_>>();
                    for index in indices {
                        let track = records.read(index)?;
                        let artist = track.scope_artist.as_deref().unwrap_or(artist);
                        let draft = sources::FileEditSession::scope_metadata(
                            record.draft.kind,
                            artist,
                            &track.draft,
                            &track.state.values(),
                        );
                        if track.state.has_changes(&track.draft) {
                            let before = sources::FileEditSession::scope_metadata(
                                record.draft.kind,
                                artist,
                                &track.draft,
                                &track.draft.values(),
                            );
                            for field in &draft.fields {
                                if before.fields.iter().any(|original| {
                                    original.key == field.key
                                        && original.value.trim() != field.value.trim()
                                }) {
                                    changed.insert(field.key.clone());
                                }
                            }
                        }
                        if let Some(combined) = &mut combined {
                            combined.merge(draft);
                        } else {
                            combined = Some(draft);
                        }
                    }
                    let mut state = record.state.clone();
                    state.identified.clear();
                    if let Some(combined) = combined {
                        for field in combined.fields {
                            state.values.insert(field.key.clone(), field.value);
                            if field.mixed {
                                state.mixed.insert(field.key.clone());
                            } else {
                                state.mixed.remove(&field.key);
                            }
                            if changed.contains(&field.key)
                                && let Some(original) = record
                                    .draft
                                    .fields
                                    .iter()
                                    .find(|original| original.key == field.key)
                            {
                                state.identified.insert(
                                    field.key,
                                    super::editor::IdentifiedField {
                                        original: original.value.clone(),
                                        original_mixed: original.mixed,
                                        touched: false,
                                    },
                                );
                            }
                        }
                    }
                    Ok((record, state))
                })
                .await?;
            state.touched.clear();
            Ok(Arc::new(MetadataEditor {
                draft: Arc::new(record.draft),
                source: self.source.clone(),
                session: self.scope.as_ref().map(|scope| scope.session.clone()),
                batch: Some(BatchScope {
                    batch: self.clone(),
                    indices: (0..self.len()).collect(),
                    has_artwork: state.current.is_some(),
                    artwork: state.artwork.clone(),
                    storage: state.storage,
                }),
                state: Mutex::new(state),
            }))
        })
        .await
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
        !self
            .records
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .changed
            .is_empty()
    }

    pub async fn tracks(
        self: Arc<Self>,
        offset: u64,
        limit: u64,
    ) -> Result<Vec<MetadataTrackSummary>, Error> {
        let runtime = self.source.shared.runtime.clone();
        super::on_runtime(runtime, async {
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
                                modified: records.changed.contains(&index),
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
        })
        .await
    }

    pub async fn editor(self: Arc<Self>, indices: Vec<u64>) -> Result<Arc<MetadataEditor>, Error> {
        let runtime = self.source.shared.runtime.clone();
        super::on_runtime(runtime, async {
            let indices = if indices.is_empty() {
                (0..self.len()).collect()
            } else {
                indices
                    .into_iter()
                    .map(|index| usize::try_from(index).map_err(|_| Error::Unavailable))
                    .collect::<Result<Vec<_>, _>>()?
            };
            self.selected_editor(indices).await
        })
        .await
    }

    pub async fn editor_except(
        self: Arc<Self>,
        excluded: Vec<u64>,
    ) -> Result<Arc<MetadataEditor>, Error> {
        let runtime = self.source.shared.runtime.clone();
        super::on_runtime(runtime, async {
            let excluded = excluded.into_iter().collect::<HashSet<_>>();
            let indices = (0..self.len())
                .filter(|index| !excluded.contains(&(*index as u64)))
                .collect();
            self.selected_editor(indices).await
        })
        .await
    }
}

impl MetadataBatch {
    pub(super) async fn save(self: &Arc<Self>, selected: &[usize]) -> Result<(), Error> {
        let changed = self
            .records
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .changed_in(selected)
            .collect::<Vec<_>>();
        let mut failures = Vec::new();
        let mut groups = HashMap::<_, Vec<usize>>::new();
        for index in changed {
            let id = if let Some(scope) = &self.scope {
                scope
                    .session
                    .source
                    .as_ref()
                    .map(|source| source.source_id().clone())
            } else {
                self.source
                    .media_source_id(self.uri(index))
                    .await
                    .map_err(Error::Write)?
            };
            groups.entry(id).or_default().push(index);
        }
        for (id, indices) in groups {
            let source = match id {
                Some(id) => match self.source.open_client(id).await {
                    Ok(source) => Some(source),
                    Err(error) => {
                        failures.push(Error::Write(error));
                        continue;
                    }
                },
                None => None,
            };
            if source.as_ref().is_some_and(|source| {
                matches!(
                    source.implementation,
                    sources::Implementation::JellyfinEmby(_) | sources::Implementation::Plex(_)
                )
            }) {
                for index in indices {
                    if let Err(error) = self.save_server(index).await {
                        failures.push(error);
                    }
                }
            } else {
                let files = sources::FileEditSession::for_tracks(source.clone());
                let files = match self.scope.as_ref().map(|scope| &scope.session.destination) {
                    Some(super::EditDestination::Files(scope)) => scope,
                    _ => &files,
                };
                if let Err(error) = self.save_files(files, source.as_ref(), indices).await {
                    failures.push(error);
                }
            }
        }
        Error::combine(failures)
    }

    async fn save_server(self: &Arc<Self>, index: usize) -> Result<(), Error> {
        let mut record = self.read(index).await?;
        let result = async {
            let edit = record
                .state
                .edit(&record.draft)
                .map_err(Error::InvalidInput)?;
            let (session, current) = self.session(index).await?;
            if current.revision != record.draft.revision {
                return Err(Error::Conflict);
            }
            session
                .save(&self.source, edit, record.state.lyrics.clone())
                .await
        }
        .await;
        let saved = matches!(result, Ok(()) | Err(Error::SavedRefreshFailed(_)));
        if saved || matches!(result, Err(Error::PartiallySaved { .. })) {
            match self.session(index).await {
                Ok((_, draft)) if saved => {
                    record.state = Editing::new(&draft);
                    record.draft = draft;
                }
                Ok((_, draft)) => record.draft.revision = draft.revision,
                Err(_) if saved => {
                    record.state.touched.clear();
                    record.state.artwork = None;
                    record.state.lyrics = None;
                }
                Err(_) => {}
            }
        }
        record.failure = result.as_ref().err().map(ToString::to_string);
        self.work(move |records| records.write(index, &record))
            .await?;
        result
    }

    fn from_uris(source: Arc<SourceOwner>, uris: Vec<Arc<str>>) -> Result<Arc<Self>, Error> {
        if uris.is_empty() {
            return Err(Error::Unavailable);
        }
        let batch = Arc::new(Self {
            scope: None,
            source,
            parent: None,
            uris,
            records: Mutex::new(Records::new()?),
        });
        Ok(batch)
    }

    async fn save_files(
        self: &Arc<Self>,
        files: &sources::FileEditSession,
        source: Option<&Arc<sources::Source>>,
        changed: Vec<usize>,
    ) -> Result<(), Error> {
        let edit = self
            .work(|records| {
                records
                    .scope
                    .as_ref()
                    .map(|scope| scope.state.edit(&scope.draft).map_err(Error::InvalidInput))
                    .transpose()
            })
            .await?;
        if changed.is_empty() {
            return Ok(());
        }
        let uris = changed
            .iter()
            .map(|index| self.uri(*index).to_owned())
            .collect();
        let artwork_changed = edit.as_ref().is_some_and(|edit| edit.artwork.is_some());
        let artist_scope = edit
            .as_ref()
            .is_some_and(|edit| edit.kind == SourceEntityKind::Artist);
        let followups = Mutex::new(Vec::new());
        let (pending, changed) = (&followups, &changed);
        let result = files
            .save_tracks(
                &self.source.shared.database,
                edit,
                uris,
                |position| async move {
                    let record = self.read(changed[position]).await?;
                    Ok((
                        record
                            .state
                            .edit(&record.draft)
                            .map_err(Error::InvalidInput)?,
                        record.draft.revision,
                        sources::artwork_file(record.draft.artwork.binding.as_deref()),
                    ))
                },
                |position, revision, outcome| async move {
                    let index = changed[position];
                    let mut record = self.read(index).await?;
                    if outcome.is_ok() {
                        let artwork_changed =
                            record.state.artwork.is_some() || record.scope_artwork;
                        let lyrics = record.state.lyrics.take();
                        if artwork_changed || lyrics.is_some() {
                            pending
                                .lock()
                                .unwrap_or_else(|e| e.into_inner())
                                .push((index, artwork_changed));
                        }
                        if artist_scope {
                            let values = sources::FileEditSession::scope_values(
                                SourceEntityKind::Artist,
                                record
                                    .scope_artist
                                    .as_deref()
                                    .unwrap_or(&self.scope.as_ref().unwrap().title),
                                &record.draft.values(),
                                &record.state.values(),
                            );
                            record.scope_artist = values.get("title").cloned();
                        }
                        record.state.apply_values(&mut record.draft);
                        record.state = Editing::new(&record.draft);
                        record.scope_artwork = false;
                        record.state.lyrics = lyrics;
                    }
                    if outcome.is_ok() || revision.is_some() {
                        record.draft.revision = revision;
                    }
                    record.failure = outcome.err().map(|error| error.to_string());
                    self.work(move |records| records.write(index, &record))
                        .await
                },
            )
            .await;
        if source.is_none() && matches!(result, Ok(()) | Err(Error::PartiallySaved { .. })) {
            self.source
                .publish_current_catalog(None, crate::runtime::CatalogChange::Broad)
                .await;
        }
        if artwork_changed {
            super::EditSession::invalidate_artwork(
                &self.source,
                source,
                self.scope
                    .as_ref()
                    .and_then(|scope| scope.session.previous_artwork.as_deref()),
                &result,
            )
            .await?;
        }
        let mut failures = Vec::new();
        for (index, artwork_changed) in followups.into_inner().unwrap_or_else(|e| e.into_inner()) {
            let mut record = self.read(index).await?;
            match self.session(index).await {
                Ok((session, draft)) => {
                    if artwork_changed {
                        super::EditSession::invalidate_artwork(
                            &self.source,
                            source,
                            record.draft.artwork.binding.as_deref(),
                            &result,
                        )
                        .await?;
                        record.draft.artwork = draft.artwork;
                        record.state.storage = record.draft.artwork.storage;
                    }
                    if let Some(lyrics) = &record.state.lyrics {
                        match session.save_lyrics(&self.source, lyrics).await {
                            Ok(revision) => {
                                record.state.lyrics = None;
                                record.draft.revision = revision;
                            }
                            Err(error) => {
                                record.failure = Some(error.to_string());
                                failures.push(error);
                            }
                        }
                    }
                }
                Err(error) if record.state.lyrics.is_some() => {
                    record.failure = Some(error.to_string());
                    failures.push(error);
                }
                Err(_) => {}
            }
            self.work(move |records| records.write(index, &record))
                .await?;
        }
        self.work(|records| {
            if records.changed.is_empty()
                && let Some(scope) = &mut records.scope
            {
                scope.state.apply_values(&mut scope.draft);
                scope.state = Editing::new(&scope.draft);
            }
            Ok(())
        })
        .await?;
        result.and(Error::combine(failures))
    }

    async fn selected_editor(
        self: Arc<Self>,
        mut indices: Vec<usize>,
    ) -> Result<Arc<MetadataEditor>, Error> {
        indices.sort_unstable();
        indices.dedup();
        if indices.iter().any(|index| *index >= self.len()) {
            return Err(Error::Unavailable);
        }
        self.ensure_loaded(&indices).await?;
        let cancelled = tokio_util::sync::CancellationToken::new();
        let _cancel = cancelled.clone().drop_guard();
        let selected = indices.clone();
        let selected_len = selected.len();
        let (draft, current, identified, artwork, has_artwork, lyrics, recordings) = self
            .work(move |records| {
                let mut identified = HashSet::new();
                let mut has_artwork = false;
                let mut lyrics = None;
                let mut recordings = Vec::new();
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
                        recordings = record.state.recordings.clone();
                    }
                    has_artwork |= match &record.state.artwork {
                        Some(ArtworkChange::Replace(_)) => true,
                        Some(ArtworkChange::Remove) => false,
                        None => record.draft.artwork.binding.is_some(),
                    };
                    identified.extend(
                        record
                            .draft
                            .fields
                            .iter()
                            .filter(|field| record.state.field_changed(field))
                            .map(|field| field.key.clone()),
                    );
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
                    record.state.apply_values(&mut draft);
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
                    recordings,
                ))
            })
            .await?;
        let mut draft = draft;
        draft.track_count = indices.len();
        let mut state = Editing::new(&current);
        state.artwork = artwork.clone();
        state.lyrics = lyrics;
        state.recordings = recordings;
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

    pub(super) async fn open_scope(
        source: Arc<SourceOwner>,
        session: Arc<super::EditSession>,
        draft: MetadataDraft,
    ) -> Result<Arc<Self>, Error> {
        let mut uris = Vec::new();
        let mut can_embed_artist = true;
        let mut after = None;
        loop {
            let page = source
                .shared
                .database
                .file_edit_rows(&session.media_uri, after, &library::ReadCancellation::new())
                .await
                .map_err(error)?;
            if page.is_empty() {
                break;
            }
            after = page.last().map(|row| row.track_key);
            can_embed_artist &= page.iter().all(|row| {
                sources::FileEditSession::can_embed_artist(row.source_format.as_deref())
            });
            uris.extend(page.into_iter().map(|row| Arc::<str>::from(row.media_uri)));
        }
        let artist = draft
            .fields
            .iter()
            .find(|field| field.key == "title")
            .map(|field| field.value.clone())
            .unwrap_or_default();
        let batch = Self::from_uris(source, uris)?;
        let mut combined: Option<MetadataDraft> = None;
        for index in 0..batch.len() {
            let track = batch.read(index).await?;
            let values = track.draft.values();
            let next = sources::FileEditSession::scope_metadata(
                draft.kind,
                &artist,
                &track.draft,
                &values,
            );
            if let Some(combined) = &mut combined {
                combined.merge(next);
            } else {
                combined = Some(next);
            }
        }
        let mut combined = combined.ok_or(Error::Unavailable)?;
        if draft.kind == SourceEntityKind::Artist {
            combined.artwork.can_embed &= can_embed_artist;
        }
        combined.artwork = combined
            .artwork
            .with_binding(draft.artwork.binding.clone(), false);
        combined.track_count = batch.len();
        for field in &draft.fields {
            if !combined
                .fields
                .iter()
                .any(|current| current.key == field.key)
            {
                combined.fields.push(field.clone());
            }
        }
        if draft.kind == SourceEntityKind::Artist {
            combined
                .fields
                .sort_by_key(|field| match field.key.as_str() {
                    "title" => 0,
                    "sort_title" => 1,
                    "musicbrainz_artist_id" => 2,
                    _ => 3,
                });
        }
        let draft = combined;
        let mut batch = Arc::try_unwrap(batch).map_err(|_| Error::Unavailable)?;
        batch.scope = Some(Arc::new(FileScope {
            session,
            title: artist,
        }));
        batch
            .records
            .get_mut()
            .unwrap_or_else(|e| e.into_inner())
            .scope = Some(Record {
            state: Editing::new(&draft),
            draft,
            failure: None,
            scope_artwork: false,
            scope_artist: None,
        });
        Ok(Arc::new(batch))
    }

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

    pub(super) async fn reset_scope(self: &Arc<Self>, key: String) -> Result<(), Error> {
        self.ensure_loaded(&(0..self.len()).collect::<Vec<_>>())
            .await?;
        self.work(move |records| {
            let scope = records.scope.as_ref().ok_or(Error::Unavailable)?;
            let kind = scope.draft.kind;
            let artist = scope
                .draft
                .fields
                .iter()
                .find(|field| field.key == "title")
                .map(|field| field.value.clone())
                .unwrap_or_default();
            let mut scope_edit = scope
                .state
                .stage_edit(&scope.draft)
                .map_err(Error::InvalidInput)?;
            scope_edit.changes = [(key.clone(), String::new())].into();
            let indices = records.offsets.keys().copied().collect::<Vec<_>>();
            for index in indices {
                let mut record = records.read(index)?;
                let artist = record.scope_artist.as_deref().unwrap_or(&artist);
                let original = Editing::new(&record.draft).values();
                let baseline =
                    sources::FileEditSession::scope_values(kind, artist, &original, &original);
                let current = sources::FileEditSession::scope_values(
                    kind,
                    artist,
                    &original,
                    &record.state.values(),
                );
                scope_edit
                    .changes
                    .insert(key.clone(), baseline.get(&key).cloned().unwrap_or_default());
                let changes = sources::FileEditSession::project_edit(
                    &record.state.values(),
                    &scope_edit,
                    current.get("title").map(String::as_str).unwrap_or(artist),
                );
                for (field, value) in changes {
                    let value = if kind == SourceEntityKind::Artist {
                        value
                    } else {
                        original.get(&field).cloned().unwrap_or_default()
                    };
                    record.state.set_text(&field, value);
                }
                records.write(index, &record)?;
            }
            if let Some(record) = &mut records.scope {
                if let Some(field) = record.draft.fields.iter().find(|field| field.key == key) {
                    record.state.values.insert(key.clone(), field.value.clone());
                    if field.mixed {
                        record.state.mixed.insert(key.clone());
                    }
                }
                record.state.touched.remove(&key);
            }
            Ok(())
        })
        .await?;
        Ok(())
    }

    pub(super) async fn reset(
        self: &Arc<Self>,
        indices: &[usize],
        key: String,
    ) -> Result<(), Error> {
        let indices = indices.to_vec();
        self.work(move |records| {
            for index in indices {
                let mut record = records.read(index)?;
                if !record.state.undo_identified(&key) {
                    if let Some(field) = record.draft.fields.iter().find(|field| field.key == key) {
                        record.state.values.insert(key.clone(), field.value.clone());
                        record.state.touched.remove(&key);
                    }
                }
                records.write(index, &record)?;
            }
            Ok(())
        })
        .await?;
        Ok(())
    }

    pub(super) async fn session(
        &self,
        index: usize,
    ) -> Result<(Arc<super::EditSession>, MetadataDraft), Error> {
        super::EditSession::open(
            &self.source,
            &MetadataItemId::Track(self.uri(index).to_string()),
        )
        .await
    }

    fn len(&self) -> usize {
        self.parent
            .as_ref()
            .map_or(self.uris.len(), |(_, indices)| indices.len())
    }

    pub(super) fn uri(&self, index: usize) -> &str {
        match &self.parent {
            Some((parent, indices)) => parent.uri(indices[index]),
            None => &self.uris[index],
        }
    }

    pub(super) fn fork(self: &Arc<Self>, indices: Vec<usize>) -> Result<Arc<Self>, Error> {
        Ok(Arc::new(Self {
            scope: self.scope.clone(),
            source: self.source.clone(),
            parent: Some((self.clone(), indices)),
            uris: Vec::new(),
            records: Mutex::new(Records::new()?),
        }))
    }

    async fn ensure_loaded(self: &Arc<Self>, indices: &[usize]) -> Result<(), Error> {
        let indices = indices.to_vec();
        let missing = self
            .work(move |records| {
                Ok(indices
                    .into_iter()
                    .filter(|index| !records.offsets.contains_key(index))
                    .collect::<Vec<_>>())
            })
            .await?;
        for index in missing {
            self.read(index).await?;
        }
        Ok(())
    }

    pub(super) async fn read(self: &Arc<Self>, index: usize) -> Result<Record, Error> {
        if let Some(record) = self
            .work(move |records| {
                records
                    .offsets
                    .contains_key(&index)
                    .then(|| records.read(index))
                    .transpose()
            })
            .await?
        {
            return Ok(record);
        }
        let record = if let Some((parent, indices)) = &self.parent {
            let mut record = Box::pin(parent.read(indices[index])).await?;
            record.state.apply_values(&mut record.draft);
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
                scope_artwork: false,
                scope_artist: None,
            }
        };
        self.work(move |records| {
            let record = if records.offsets.contains_key(&index) {
                records.read(index)?
            } else {
                records.write(index, &record)?;
                record
            };
            Ok(record)
        })
        .await
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
                Some(self.session(index).await?.0)
            } else {
                None
            },
            batch: None,
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
        let Some((parent, targets)) = &self.parent else {
            return Ok(());
        };
        let loaded = self
            .work(|records| Ok(records.offsets.keys().copied().collect::<Vec<_>>()))
            .await?;
        for index in loaded {
            let mut reviewed = self.read(index).await?;
            let chosen = reviewed.state.lookup_choice.is_some();
            if !chosen && reviewed.state.lookup_candidates.is_none() {
                continue;
            }
            if chosen {
                reviewed
                    .state
                    .apply_lookup_choice(&reviewed.draft, fill_missing);
            }
            let target = targets[index];
            parent
                .work(move |records| {
                    let mut original = records.read(target)?;
                    if chosen {
                        original.state = reviewed.state;
                    } else {
                        original.state.recordings = reviewed.state.recordings;
                        original.state.lookup_candidates = reviewed.state.lookup_candidates;
                        original.state.scan_error = reviewed.state.scan_error;
                    }
                    records.write(target, &original)
                })
                .await?;
        }
        Ok(())
    }

    pub(super) async fn stage(self: &Arc<Self>, editor: &MetadataEditor) -> Result<(), Error> {
        let scope = editor.batch.as_ref().unwrap();
        let scoped = editor.draft.kind != SourceEntityKind::Track;
        let artwork_changed = scoped
            && (editor.state().artwork != scope.artwork || editor.state().storage != scope.storage);
        let mut edit = editor
            .state()
            .stage_edit(&editor.draft)
            .map_err(Error::InvalidInput)?;
        if edit.artwork.as_ref().is_some_and(|artwork| {
            scope.artwork.as_ref() == Some(&artwork.change) && scope.storage == artwork.storage
        }) {
            edit.artwork = None;
        }
        let lyrics = editor.state().lyrics.clone();
        if edit.changes.is_empty() && edit.artwork.is_none() && lyrics.is_none() && !artwork_changed
        {
            return Ok(());
        }
        self.ensure_loaded(&scope.indices).await?;
        let next_scope = scoped.then(|| editor.state().clone());
        let editor_artwork_pending = editor.state().artwork.is_some();
        let indices = scope.indices.clone();
        let cancelled = tokio_util::sync::CancellationToken::new();
        let _cancel = cancelled.clone().drop_guard();
        self.work(move |records| {
            let previous_artist = records
                .scope
                .as_ref()
                .and_then(|record| {
                    record
                        .draft
                        .fields
                        .iter()
                        .find(|field| field.key == "title")
                })
                .map(|field| field.value.clone())
                .unwrap_or_default();
            for index in indices {
                if cancelled.is_cancelled() {
                    return Err(Error::Unavailable);
                }
                let mut record = records.read(index)?;
                let mut changed = lyrics.is_some() || artwork_changed;
                if artwork_changed {
                    record.scope_artwork = editor_artwork_pending;
                }
                let changes = if scoped {
                    let artist = record.scope_artist.as_deref().unwrap_or(&previous_artist);
                    let values = sources::FileEditSession::scope_values(
                        edit.kind,
                        artist,
                        &Editing::new(&record.draft).values(),
                        &record.state.values(),
                    );
                    sources::FileEditSession::project_edit(
                        &record.state.values(),
                        &edit,
                        values.get("title").map(String::as_str).unwrap_or(artist),
                    )
                } else {
                    edit.changes.clone()
                };
                for (key, value) in &changes {
                    if record.state.values.get(key) != Some(value) {
                        changed = true;
                        record.state.set_text(key, value.clone());
                    }
                }
                if let Some(artwork) = &edit.artwork
                    && !scoped
                {
                    let storage = if record.draft.artwork.storage == sources::ArtworkStorage::Server
                    {
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
            if let Some(next) = next_scope
                && let Some(record) = &mut records.scope
            {
                for (key, value) in &edit.changes {
                    record.state.set_text(key, value.clone());
                }
                record.state.artwork = next.artwork;
                record.state.storage = next.storage;
            }
            Ok(())
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
        Ok(())
    }
}

impl MetadataEditor {
    pub(super) async fn reload_batch(&self) -> Result<(), Error> {
        if self.batch.is_some() {
            let editor = self.reload().await?;
            let mut next = editor.state().clone();
            let mut state = self.state();
            next.current = state.current.clone();
            next.artwork_revision = state.artwork_revision.wrapping_add(1);
            *state = next;
        }
        Ok(())
    }
}
