use std::path::{Path, PathBuf};
use std::sync::Arc;

use library::{Database, ReadCancellation};
use sqlx::{Connection, Row};

use super::{Implementation, Source};
use crate::file::metadata::{self as tags, TagUpdate};
use crate::{MetadataDraft, MetadataEdit, SourceEntityKind, SourceMetadataError as Error};

fn failure(error: impl std::fmt::Display) -> Error {
    Error::Write(error.to_string())
}

#[derive(Clone)]
enum FileLocation {
    Native(PathBuf),
    Document {
        uri: String,
        root: String,
        relative: String,
    },
    Remote {
        source: Arc<Source>,
        path: String,
    },
}

#[derive(Clone)]
pub(crate) struct NativeFile {
    location: FileLocation,
    format: Option<String>,
    cue: bool,
    media_uri: String,
}

pub(crate) struct PreparedFile {
    target: NativeFile,
    file: tempfile::TempPath,
    revision: Option<String>,
}

impl NativeFile {
    pub(crate) fn direct(uri: &str) -> Result<Self, Error> {
        let uri = url::Url::parse(uri).map_err(|_| Error::Unavailable)?;
        if uri.fragment().is_some() || uri.query().is_some() {
            return Err(Error::Unavailable);
        }
        let path = uri.to_file_path().map_err(|_| Error::Unavailable)?;
        Ok(Self {
            location: FileLocation::Native(path),
            format: None,
            cue: false,
            media_uri: uri.into(),
        })
    }

    fn resolve(
        source: &Arc<Source>,
        media_uri: &str,
        source_path: Option<&str>,
        format: Option<&str>,
        cue: bool,
        original: Option<library::LocalLocatorWrite>,
    ) -> Result<Self, Error> {
        let location = if source.file_source(source_path).is_some() {
            FileLocation::Remote {
                source: Arc::clone(source),
                path: source_path.map(str::to_owned).ok_or(Error::Unavailable)?,
            }
        } else if let Some(original) = original {
            if original.access_uri.starts_with("content:") || original.path.starts_with("content:")
            {
                FileLocation::Document {
                    uri: original.access_uri,
                    root: original.root,
                    relative: original.relative_path,
                }
            } else {
                FileLocation::Native(PathBuf::from(original.path))
            }
        } else if matches!(source.implementation, Implementation::Local(_)) {
            FileLocation::Native(PathBuf::from(source_path.ok_or(Error::Unavailable)?))
        } else if matches!(source.implementation, Implementation::OpenSubsonic(_)) {
            return Err(Error::LocalAccessRequired {
                source_id: source.source_id.clone(),
                source_path: source_path.map(str::to_owned).ok_or(Error::Unavailable)?,
            });
        } else {
            return Err(Error::Unavailable);
        };
        Ok(Self {
            location,
            format: format.map(str::to_owned),
            cue: cue,
            media_uri: media_uri.to_owned(),
        })
    }

    async fn revision(&self) -> Result<Option<String>, Error> {
        match &self.location {
            FileLocation::Native(path) => {
                let path = path.clone();
                tokio::task::spawn_blocking(move || tags::revision(&path).map(Some))
                    .await
                    .map_err(failure)?
            }
            FileLocation::Document { uri, .. } => {
                let uri = uri.clone();
                tokio::task::spawn_blocking(move || {
                    crate::stat_document(&uri)
                        .map(|entry| entry.revision)
                        .map_err(failure)
                })
                .await
                .map_err(failure)?
            }
            FileLocation::Remote { source, path } => {
                let files = source.file_source(Some(path)).ok_or(Error::Unavailable)?;
                let input = files.input().await.map_err(failure)?;
                Ok(files
                    .stat(&input, &files.relative(path).map_err(failure)?)
                    .await
                    .map_err(failure)?
                    .revision)
            }
        }
    }

    async fn read(&self) -> Result<MetadataDraft, Error> {
        let revision = self.revision().await?;
        let mut draft = match &self.location {
            FileLocation::Native(path) => {
                let path = path.clone();
                tokio::task::spawn_blocking(move || {
                    tags::read_metadata_input(std::fs::File::open(&path).map_err(failure)?, &path)
                })
                .await
                .map_err(failure)??
            }
            FileLocation::Document { uri, .. } => {
                let uri = uri.clone();
                tokio::task::spawn_blocking(move || {
                    let entry = crate::stat_document(&uri).map_err(failure)?;
                    let mut draft = tags::read_metadata_input(
                        crate::open_document_input(&uri).map_err(failure)?,
                        Path::new(&entry.name),
                    )?;
                    if !entry.writable {
                        for field in &mut draft.fields {
                            field.writable = false;
                        }
                        draft.artwork.can_embed = false;
                    }
                    Ok::<_, Error>(draft)
                })
                .await
                .map_err(failure)??
            }
            FileLocation::Remote { source, path } => {
                let files = source.file_source(Some(path)).ok_or(Error::Unavailable)?;
                let relative = files.relative(path).map_err(failure)?;
                let input = files.input().await.map_err(failure)?;
                let reader = crate::file::remote::reader::FileReader::open(
                    &input,
                    &files
                        .input_path(input.input(), &relative)
                        .map_err(failure)?,
                    revision.as_deref().unwrap_or_default(),
                    &self.media_uri,
                )
                .await
                .map_err(failure)?;
                let writable = if let Some(access) = files.document_access() {
                    let access = Arc::clone(access);
                    let relative = relative.clone();
                    tokio::task::spawn_blocking(move || access.resolve(&relative))
                        .await
                        .map_err(failure)?
                        .map_err(failure)?
                        .writable
                } else {
                    true
                };
                tokio::task::spawn_blocking(move || {
                    let mut draft = tags::read_metadata_input(
                        reader,
                        Path::new(&crate::file::documents::display_path(&relative)),
                    )?;
                    if !writable {
                        for field in &mut draft.fields {
                            field.writable = false;
                        }
                        draft.artwork.can_embed = false;
                    }
                    Ok::<_, Error>(draft)
                })
                .await
                .map_err(failure)??
            }
        };
        if self.revision().await? != revision {
            return Err(Error::Conflict);
        }
        draft.revision = revision;
        if self.cue {
            for field in &mut draft.fields {
                field.writable = false;
            }
            draft.artwork.can_embed = false;
        }
        Ok(draft)
    }

    fn lyrics_storage(&self) -> Vec<crate::LyricsStorage> {
        if self.cue {
            return Vec::new();
        }
        let sidecar = match &self.location {
            FileLocation::Native(path) => path.parent().is_some_and(Path::is_dir),
            _ => self.folder_identity(None).is_ok(),
        };
        let embedded = (|| match &self.location {
            FileLocation::Native(path) => tags::embedded_lyrics_writable(path),
            FileLocation::Document { uri, .. } => {
                crate::stat_document(uri).is_ok_and(|entry| entry.writable)
                    && self
                        .format
                        .as_deref()
                        .is_some_and(tags::embedded_lyrics_format_writable)
            }
            FileLocation::Remote { source, path } => {
                let Some(files) = source.file_source(Some(path)) else {
                    return false;
                };
                if let Some(access) = files.document_access() {
                    let Ok(relative) = files.relative(path) else {
                        return false;
                    };
                    if !access.resolve(&relative).is_ok_and(|entry| entry.writable) {
                        return false;
                    }
                }
                self.format
                    .as_deref()
                    .is_some_and(tags::embedded_lyrics_format_writable)
            }
        })();
        let mut options = Vec::new();
        if embedded {
            options.push(crate::LyricsStorage::Embedded);
        }
        if sidecar {
            options.push(crate::LyricsStorage::Sidecar);
        }
        options
    }

    async fn save_lyrics(
        &self,
        text: &str,
        sidecar: bool,
        cancelled: impl Fn() -> bool + Send + Sync,
    ) -> Result<Option<PreparedFile>, Error> {
        if sidecar {
            if cancelled() {
                return Err(crate::SourceError::Cancelled.into());
            }
            self.write_sidecar(text).await?;
            Ok(None)
        } else {
            let (prepared, _) = self
                .prepare(None)
                .await?
                .mutate(TagUpdate::Lyrics(text.into()), None)
                .await?;
            Ok(Some(prepared.commit(&cancelled).await?))
        }
    }

    fn analysis_key(&self, revision: Option<&str>) -> [u8; 32] {
        let mut hash = blake3::Hasher::new();
        hash.update(self.media_uri.as_bytes());
        hash.update(revision.unwrap_or_default().as_bytes());
        *hash.finalize().as_bytes()
    }

    pub(crate) async fn prepare(&self, expected: Option<&str>) -> Result<PreparedFile, Error> {
        if self.cue {
            return Err(Error::Unavailable);
        }
        let before = self.revision().await?;
        if expected.is_some_and(|expected| Some(expected) != before.as_deref()) {
            return Err(Error::Conflict);
        }
        let file = match &self.location {
            FileLocation::Native(path) => {
                let path = path.clone();
                tokio::task::spawn_blocking(move || {
                    let file = tempfile::Builder::new()
                        .prefix(tags::WORKING_COPY_PREFIX)
                        .tempfile_in(path.parent().ok_or(Error::Unavailable)?)
                        .map_err(failure)?;
                    std::fs::copy(&path, file.path()).map_err(failure)?;
                    Ok::<_, Error>(file.into_temp_path())
                })
                .await
                .map_err(failure)??
            }
            FileLocation::Document { uri, .. } => {
                let uri = uri.clone();
                tokio::task::spawn_blocking(move || {
                    let file = tempfile::NamedTempFile::new()
                        .map_err(failure)?
                        .into_temp_path();
                    crate::copy_document(&uri, &file).map_err(failure)?;
                    Ok::<_, Error>(file)
                })
                .await
                .map_err(failure)??
            }
            FileLocation::Remote { source, path } => {
                let file = tempfile::NamedTempFile::new()
                    .map_err(failure)?
                    .into_temp_path();
                let files = source.file_source(Some(path)).ok_or(Error::Unavailable)?;
                files
                    .copy_file(&files.relative(path).map_err(failure)?, &file)
                    .await
                    .map_err(failure)?
                    .ok_or(Error::Unavailable)?;
                file
            }
        };
        if self.revision().await? != before {
            return Err(Error::Conflict);
        }
        Ok(PreparedFile {
            target: self.clone(),
            file,
            revision: before,
        })
    }
}

impl PreparedFile {
    pub(crate) async fn mutate(
        self,
        update: TagUpdate,
        mut mp4: tags::MetadataImageCache,
    ) -> Result<(Self, tags::MetadataImageCache), Error> {
        if self.target.cue {
            return Err(Error::Unavailable);
        }
        tokio::task::spawn_blocking(move || {
            tags::edit_copy(&self.file, self.target.format.as_deref(), &update, &mut mp4)?;
            std::fs::File::options()
                .write(true)
                .open(&self.file)
                .and_then(|file| file.sync_all())
                .map_err(failure)?;
            Ok((self, mp4))
        })
        .await
        .map_err(failure)?
    }

    pub(crate) async fn commit(
        mut self,
        cancelled: &(dyn Fn() -> bool + Send + Sync),
    ) -> Result<Self, Error> {
        if self.target.revision().await? != self.revision {
            return Err(Error::Conflict);
        }
        if cancelled() {
            return Err(crate::SourceError::Cancelled.into());
        }
        match &self.target.location {
            FileLocation::Native(path) => {
                let path = path.clone();
                self = tokio::task::spawn_blocking(move || {
                    tags::replace(&mut self.file, &path)?;
                    tags::sync_parent(path.parent().ok_or(Error::Unavailable)?).map_err(
                        |error| Error::PartiallySaved {
                            reason: error.to_string(),
                        },
                    )?;
                    Ok::<_, Error>(self)
                })
                .await
                .map_err(failure)??;
            }
            FileLocation::Document { uri, .. } => {
                let uri = uri.clone();
                let path = self.file.to_path_buf();
                let revision = self.revision.clone();
                tokio::task::spawn_blocking(move || {
                    crate::save_document(&uri, &path, revision.as_deref()).map_err(failure)
                })
                .await
                .map_err(failure)??;
            }
            FileLocation::Remote { source, path } => {
                let files = source.file_source(Some(path)).ok_or(Error::Unavailable)?;
                files
                    .commit_file(
                        &files.relative(path).map_err(failure)?,
                        &self.file,
                        self.revision.as_deref(),
                        true,
                    )
                    .await?;
            }
        }
        Ok(self)
    }

    pub(crate) async fn preserve_fingerprint(&self, database: &Database) -> Result<(), Error> {
        let previous = self.target.analysis_key(self.revision.as_deref());
        let current = self
            .target
            .analysis_key(self.target.revision().await?.as_deref());
        database
            .preserve_fingerprint(&self.target.media_uri, &previous, &current)
            .await
            .map_err(failure)
    }

    async fn read_catalog(
        &self,
        source: &Source,
        catalog: &library::Catalog<'_>,
    ) -> Result<SavedFile, Error> {
        self.preserve_fingerprint(catalog.database).await?;
        let previous = catalog
            .database
            .audio_analysis_key(&self.target.media_uri)
            .await
            .map_err(failure)?;
        let result = async {
            let (observation, facts) = match &self.target.location {
                FileLocation::Remote { source, path } => {
                    let files = source.file_source(Some(path)).ok_or(Error::Unavailable)?;
                    let input = files.input().await.map_err(failure)?;
                    let (observation, facts) = files
                        .read_saved_file(
                            &self.target.media_uri,
                            &files.relative(path).map_err(failure)?,
                            &input,
                            &self.file,
                        )
                        .await
                        .map_err(failure)?;
                    (Some(observation), facts)
                }
                FileLocation::Native(path) => {
                    crate::file::local::scan::read_saved_file(catalog, &self.target.media_uri, path)
                        .await
                        .map_err(failure)?
                }
                FileLocation::Document { uri, .. } => {
                    crate::file::local::scan::read_saved_document(
                        catalog,
                        &self.target.media_uri,
                        uri,
                        &self.file,
                    )
                    .await
                    .map_err(failure)?
                }
            };
            if source.is_retired() {
                return Err(crate::SourceError::Cancelled.into());
            }
            Ok::<_, Error>(SavedFile {
                uri: self.target.media_uri.clone(),
                observation,
                facts,
                previous,
            })
        }
        .await;
        result.map_err(|error| Error::SavedRefreshFailed(error.to_string()))
    }

    async fn write_catalog(
        &self,
        source: &Source,
        catalog: &mut library::Catalog<'_>,
    ) -> Result<(), Error> {
        let saved = self.read_catalog(source, catalog).await?;
        publish_saved_files(catalog, &mut vec![saved]).await
    }
}

struct SavedFile {
    uri: String,
    observation: Option<library::LocalFileWrite>,
    facts: library::TrackObservation,
    previous: Option<[u8; 32]>,
}

async fn publish_saved_files(
    catalog: &mut library::Catalog<'_>,
    pending: &mut Vec<SavedFile>,
) -> Result<(), Error> {
    if pending.is_empty() {
        return Ok(());
    }
    let mut files = Vec::new();
    let mut tracks = Vec::new();
    let mut analysis = Vec::new();
    for saved in pending.drain(..) {
        if let Some(file) = saved.observation {
            files.push((file, Vec::new()));
        }
        if let Some(previous) = saved.previous {
            analysis.push((saved.uri, previous));
        }
        tracks.push(saved.facts);
    }
    catalog
        .write_file_observations(&files, tracks, true)
        .await
        .map_err(|error| Error::SavedRefreshFailed(error.to_string()))?;
    for (uri, previous) in analysis {
        catalog
            .database
            .preserve_audio_analysis(&uri, &previous)
            .await
            .map_err(|error| Error::SavedRefreshFailed(error.to_string()))?;
    }
    Ok(())
}

struct SaveWork(sqlx::SqliteConnection);

impl SaveWork {
    async fn new() -> Result<Self, Error> {
        let options = sqlx::sqlite::SqliteConnectOptions::new()
            .filename("")
            .create_if_missing(true)
            .pragma("temp_store", "FILE")
            .journal_mode(sqlx::sqlite::SqliteJournalMode::Off)
            .synchronous(sqlx::sqlite::SqliteSynchronous::Off);
        let mut connection = sqlx::SqliteConnection::connect_with(&options)
            .await
            .map_err(failure)?;
        sqlx::raw_sql("CREATE TABLE folders(identity TEXT PRIMARY KEY); CREATE TABLE members(member INTEGER PRIMARY KEY,object_id TEXT NOT NULL)")
            .execute(&mut connection).await.map_err(failure)?;
        Ok(Self(connection))
    }

    async fn remember(&mut self, page: &[(i64, Option<String>, NativeFile)]) -> Result<(), Error> {
        let members = page
            .iter()
            .filter_map(|(key, id, _)| id.as_ref().map(|id| (*key, id)))
            .collect::<Vec<_>>();
        if members.is_empty() {
            return Ok(());
        }
        let mut insert =
            sqlx::QueryBuilder::new("INSERT OR IGNORE INTO members(member,object_id) ");
        insert.push_values(members, |mut row, (key, id)| {
            row.push_bind(key).push_bind(id);
        });
        insert.build().execute(&mut self.0).await.map_err(failure)?;
        Ok(())
    }

    async fn first_folder(&mut self, identity: &str) -> Result<bool, Error> {
        Ok(sqlx::query("INSERT OR IGNORE INTO folders VALUES(?1)")
            .bind(identity)
            .execute(&mut self.0)
            .await
            .map_err(failure)?
            .rows_affected()
            > 0)
    }
}

#[derive(Clone)]
pub struct FileEditSession {
    source: Option<Arc<Source>>,
    media_uri: String,
    previous_artist: String,
    folder_image: Option<String>,
    single: Option<NativeFile>,
    expected: Option<String>,
}

impl FileEditSession {
    pub async fn audio_revision(&self) -> Result<Option<[u8; 32]>, Error> {
        let file = self.single.as_ref().ok_or(Error::Unavailable)?;
        Ok(file
            .revision()
            .await?
            .map(|revision| file.analysis_key(Some(&revision))))
    }

    pub fn lyrics_storage(&self) -> Vec<crate::LyricsStorage> {
        self.single
            .as_ref()
            .map(NativeFile::lyrics_storage)
            .unwrap_or_default()
    }

    pub async fn save_lyrics(
        &self,
        database: &Database,
        text: &str,
        sidecar: bool,
    ) -> Result<(), Error> {
        if let Some(source) = &self.source {
            source
                .write_lyrics(database, &self.media_uri, text, sidecar)
                .await?;
            source.refresh_written_lyrics(database);
            return Ok(());
        }
        let file = self.single.as_ref().ok_or(Error::Unavailable)?;
        if let Some(saved) = file.save_lyrics(text, sidecar, || false).await? {
            saved.preserve_fingerprint(database).await?;
        }
        if !sidecar {
            file.publish_direct(database).await?;
        }
        database
            .remove_track_lyrics_by_authority(&self.media_uri, "source")
            .await
            .map_err(failure)?;
        Ok(())
    }

    pub fn media_uri(&self) -> &str {
        &self.media_uri
    }

    pub async fn open(
        source: Option<Arc<Source>>,
        database: &Database,
        media_uri: String,
        kind: SourceEntityKind,
    ) -> Result<(Self, MetadataDraft), Error> {
        let cancel = ReadCancellation::new();
        let (base, mut binding, previous_artist, single) = match kind {
            SourceEntityKind::Track => {
                let row = database
                    .track(&(&media_uri), &cancel)
                    .await
                    .map_err(failure)?;
                let (file, base, binding) = if let Some(row) = row {
                    let binding = if row.artwork_binding.is_some() {
                        row.artwork_binding.clone()
                    } else if let Some(album) = &row.links.album_media_uri {
                        database
                            .album(&(album), false, &cancel)
                            .await
                            .map_err(failure)?
                            .and_then(|album| album.artwork_binding)
                    } else {
                        None
                    };
                    let file = if let Some(source) = &source {
                        NativeFile::resolve(
                            source,
                            &row.media_uri,
                            row.access.source_path.as_deref(),
                            row.metadata.source_format.as_deref(),
                            row.access.cue_path.is_some(),
                            database.original_file(&media_uri).await.map_err(failure)?,
                        )?
                    } else {
                        NativeFile::direct(&media_uri)?
                    };
                    (file, tags::readonly_track_metadata(&row, None), binding)
                } else {
                    (
                        NativeFile::direct(&media_uri)?,
                        MetadataDraft::new(kind, Vec::new()),
                        None,
                    )
                };
                (base, binding, String::new(), Some(file))
            }
            SourceEntityKind::Album => {
                let row = database
                    .album(&(&media_uri), false, &cancel)
                    .await
                    .map_err(failure)?
                    .ok_or(Error::Unavailable)?;
                (
                    tags::readonly_album_metadata(&row, None),
                    row.artwork_binding,
                    String::new(),
                    None,
                )
            }
            SourceEntityKind::Artist => {
                let row = database
                    .artist(&(&media_uri), false, false, &cancel)
                    .await
                    .map_err(failure)?
                    .ok_or(Error::Unavailable)?;
                (
                    tags::readonly_artist_metadata(&row, None),
                    row.artwork_binding,
                    row.metadata.name,
                    None,
                )
            }
        };
        if binding.is_none() && source.is_none() {
            if let Some(NativeFile {
                location: FileLocation::Native(path),
                ..
            }) = &single
            {
                let path = path.clone();
                binding = tokio::task::spawn_blocking(move || {
                    let image = path
                        .parent()
                        .and_then(crate::file::local::artwork::directory_image)
                        .map(|image| {
                            crate::file::local::artwork::file_reference(
                                "local",
                                &image,
                                tags::revision(&image).unwrap_or_default(),
                            )
                        })
                        .or_else(|| {
                            crate::file::local::artwork::inspect_embedded(
                                "local",
                                &mut crate::file::discovery::Reader::default(),
                                &path,
                                tags::revision(&path).unwrap_or_default(),
                            )
                        });
                    image.map(|image| image.artwork_binding())
                })
                .await
                .map_err(failure)?;
            }
        }
        let folder_image = crate::artwork_file(binding.as_deref());
        let mut session = Self {
            source,
            media_uri,
            previous_artist,
            folder_image,
            single,
            expected: None,
        };
        let mut draft = if let Some(file) = &session.single {
            let draft = if file.cue {
                base.clone()
            } else {
                file.read().await?
            };
            session.expected = draft.revision.clone();
            if draft.fields.iter().any(|field| field.writable) || base.fields.is_empty() {
                draft
            } else {
                base.clone()
            }
        } else {
            base.clone()
        };
        for field in &base.fields {
            if field.key.starts_with("musicbrainz_") {
                draft.suggest(&field.key, Some(field.value.clone()));
            }
        }
        draft.revision = session.expected.clone();
        draft.artwork = draft.artwork.with_binding(binding, false);
        Ok((session, draft))
    }

    pub async fn save(&self, database: &Database, edit: MetadataEdit) -> Result<(), Error> {
        if edit.changes.is_empty() && edit.artwork.is_none() {
            return Ok(());
        }
        let file = self.single.as_ref().ok_or(Error::Unavailable)?;
        self.save_tracks(
            database,
            edit,
            vec![file.media_uri.clone()],
            |_| async {
                Ok(Some((
                    MetadataEdit {
                        kind: SourceEntityKind::Track,
                        changes: Default::default(),
                        artwork: None,
                    },
                    None,
                )))
            },
            |_, _| async { Ok(()) },
        )
        .await
    }

    pub async fn save_tracks<F, Fut, C, Done>(
        &self,
        database: &Database,
        edit: MetadataEdit,
        tracks: Vec<String>,
        mut extra: F,
        mut complete: C,
    ) -> Result<(), Error>
    where
        F: FnMut(String) -> Fut,
        Fut: std::future::Future<Output = Result<Option<(MetadataEdit, Option<String>)>, Error>>,
        C: FnMut(String, Result<Option<String>, String>) -> Done,
        Done: std::future::Future<Output = Result<(), Error>>,
    {
        let edits = [(
            edit,
            self.previous_artist.clone(),
            self.folder_image.clone(),
        )]
        .into_iter()
        .filter(|(edit, _, _)| !edit.changes.is_empty() || edit.artwork.is_some())
        .collect::<Vec<_>>();
        if edits.iter().any(|(edit, _, _)| {
            edit.artwork
                .as_ref()
                .is_some_and(|art| art.storage == crate::ArtworkStorage::Server)
        }) {
            return Err(Error::Unavailable);
        }
        let mut catalog = match &self.source {
            Some(source) => Some(
                database
                    .catalog(source.source_id().as_str(), None)
                    .await
                    .map_err(failure)?,
            ),
            None => None,
        };
        let cancelled = || {
            self.source
                .as_ref()
                .is_some_and(|source| source.is_retired())
        };
        if cancelled() {
            return Err(crate::SourceError::Cancelled.into());
        }
        let mut saved = false;
        let mut mp4 = None;
        let mut pending = Vec::new();
        let mapped = self
            .source
            .as_ref()
            .is_some_and(|source| matches!(source.implementation, Implementation::OpenSubsonic(_)));
        let folder_changed = edits.iter().any(|(edit, artist, hint)| {
            edit.artwork
                .as_ref()
                .is_some_and(|art| art.storage == crate::ArtworkStorage::Folder)
                || tags::artist_image_needs_rename(edit, artist, hint.as_deref().map(Path::new))
        });
        let mut work = if mapped || folder_changed {
            Some(SaveWork::new().await?)
        } else {
            None
        };
        let scope_tags_changed = edits.iter().any(|(edit, _, _)| {
            !edit.changes.is_empty()
                || edit
                    .artwork
                    .as_ref()
                    .is_some_and(|art| art.storage == crate::ArtworkStorage::Embedded)
        });
        let mut selected = tracks.chunks(128);
        let mut ordinal = 0i64;
        let result = async {
            loop {
                let Some(uris) = selected.next() else { break };
                let mut page = Vec::with_capacity(uris.len());
                for uri in uris {
                    let (file, id) = if let Some(file) =
                        self.single.as_ref().filter(|file| file.media_uri == *uri)
                    {
                        (
                            file.clone(),
                            library::source_entity_parts(uri).map(|(_, _, id)| id),
                        )
                    } else if let Some(source) = &self.source {
                        let (file, row) = source.resolved_track(database, uri).await?;
                        (file, row.object_id)
                    } else {
                        (NativeFile::direct(uri)?, None)
                    };
                    page.push((ordinal, id, file));
                    ordinal += 1;
                }
                if mapped {
                    work.as_mut().unwrap().remember(&page).await?;
                }
                for (_, _, file) in page {
                    let Some((track_edit, expected)) = extra(file.media_uri.clone()).await? else {
                        continue;
                    };
                    let tags_changed = scope_tags_changed
                        || !track_edit.changes.is_empty()
                        || track_edit
                            .artwork
                            .as_ref()
                            .is_some_and(|art| art.storage == crate::ArtworkStorage::Embedded);
                    let mut edits = edits.clone();
                    if !track_edit.changes.is_empty() || track_edit.artwork.is_some() {
                        edits.push((track_edit, String::new(), self.folder_image.clone()));
                    }
                    let folder_changed = folder_changed
                        || edits.iter().any(|(edit, _, _)| {
                            edit.artwork
                                .as_ref()
                                .is_some_and(|art| art.storage == crate::ArtworkStorage::Folder)
                        });
                    if folder_changed && work.is_none() {
                        work = Some(SaveWork::new().await?);
                    }
                    let file_result = async {
                        if tags_changed {
                            let prepared =
                                file.prepare(expected.as_deref().or_else(|| {
                                    self.single.as_ref().and(self.expected.as_deref())
                                }))
                                .await?;
                            let (prepared, next) = prepared
                                .mutate(
                                    TagUpdate::Metadata(
                                        edits
                                            .iter()
                                            .map(|(edit, artist, _)| (edit.clone(), artist.clone()))
                                            .collect(),
                                    ),
                                    mp4.take(),
                                )
                                .await?;
                            mp4 = next;
                            let committed = match prepared.commit(&cancelled).await {
                                Ok(committed) => committed,
                                Err(error @ Error::PartiallySaved { .. }) => {
                                    saved = true;
                                    if !mapped && self.source.is_some() {
                                        let path = match &file.location {
                                            FileLocation::Native(path) => {
                                                path.to_string_lossy().into_owned()
                                            }
                                            FileLocation::Document { uri, .. } => uri.clone(),
                                            FileLocation::Remote { path, .. } => path.clone(),
                                        };
                                        let source = self.source.as_ref().unwrap();
                                        let refresh = source
                                            .acquire_paths(
                                                catalog.as_mut().unwrap(),
                                                &[path],
                                                None,
                                                &|| false,
                                            )
                                            .await
                                            .map_err(|error| {
                                                Error::SavedRefreshFailed(error.to_string())
                                            });
                                        return crate::finish_metadata_save(
                                            Err(error),
                                            refresh,
                                            true,
                                        )
                                        .map(|()| None);
                                    }
                                    if self.source.is_none() {
                                        return crate::finish_metadata_save(
                                            Err(error),
                                            file.publish_direct(database).await,
                                            true,
                                        )
                                        .map(|()| None);
                                    }
                                    return Err(error);
                                }
                                Err(error) => return Err(error),
                            };
                            saved = true;
                            if mapped || self.source.is_none() {
                                committed.preserve_fingerprint(database).await?;
                            }
                            if !mapped && let Some(source) = &self.source {
                                pending.push(
                                    committed
                                        .read_catalog(source, catalog.as_ref().unwrap())
                                        .await?,
                                );
                                if pending.len() == 128 {
                                    publish_saved_files(catalog.as_mut().unwrap(), &mut pending)
                                        .await?;
                                }
                            } else if self.source.is_none() {
                                file.publish_direct(database).await.map_err(|error| {
                                    Error::SavedRefreshFailed(error.to_string())
                                })?;
                            }
                        }
                        for (edit, artist, hint) in &edits {
                            if !(edit
                                .artwork
                                .as_ref()
                                .is_some_and(|art| art.storage == crate::ArtworkStorage::Folder)
                                || tags::artist_image_needs_rename(
                                    edit,
                                    artist,
                                    hint.as_deref().map(Path::new),
                                ))
                            {
                                continue;
                            }
                            let directory = file.folder_identity(hint.as_deref())?;
                            if work
                                .as_mut()
                                .unwrap()
                                .first_folder(&format!("{}:{directory}", edit.kind.as_str()))
                                .await?
                            {
                                if cancelled() {
                                    return Err(crate::SourceError::Cancelled.into());
                                }
                                let paths = match file
                                    .write_folder_artwork(database, edit, hint.as_deref(), artist)
                                    .await
                                {
                                    Ok(paths) => paths,
                                    Err(error @ Error::PartiallySaved { .. }) => {
                                        saved = true;
                                        if !mapped && self.source.is_some() {
                                            let source = self.source.as_ref().unwrap();
                                            let refresh = source
                                                .acquire_paths(
                                                    catalog.as_mut().unwrap(),
                                                    &[directory],
                                                    None,
                                                    &|| false,
                                                )
                                                .await
                                                .map_err(|error| {
                                                    Error::SavedRefreshFailed(error.to_string())
                                                });
                                            return crate::finish_metadata_save(
                                                Err(error),
                                                refresh,
                                                true,
                                            )
                                            .map(|()| None);
                                        }
                                        return Err(error);
                                    }
                                    Err(error) => return Err(error),
                                };
                                saved = true;
                                if !mapped && self.source.is_some() && !paths.is_empty() {
                                    self.source
                                        .as_ref()
                                        .unwrap()
                                        .acquire_paths(
                                            catalog.as_mut().unwrap(),
                                            &paths,
                                            None,
                                            &|| false,
                                        )
                                        .await
                                        .map_err(|error| {
                                            Error::SavedRefreshFailed(error.to_string())
                                        })?;
                                }
                            }
                        }
                        file.revision().await
                    }
                    .await;
                    complete(
                        file.media_uri.clone(),
                        file_result.as_ref().cloned().map_err(ToString::to_string),
                    )
                    .await?;
                    file_result?;
                }
            }
            Ok::<_, Error>(())
        }
        .await;
        let publication = if !pending.is_empty() {
            publish_saved_files(catalog.as_mut().unwrap(), &mut pending).await
        } else {
            Ok(())
        };
        if mapped && saved {
            self.source
                .as_ref()
                .unwrap()
                .refresh_metadata_after_write(database, work.take());
        }
        if let Some(source) = &self.source
            && !source.is_retired()
            && let Some(catalog) = catalog
        {
            database
                .publish_catalog_update(source.source_id().as_str(), catalog.into_changes())
                .await;
        }
        crate::finish_metadata_save(result, publication, saved)
    }

    pub fn project_edit(
        values: &crate::MetadataChanges,
        edit: &MetadataEdit,
        previous_artist: &str,
    ) -> crate::MetadataChanges {
        tags::project_edit(values, edit, previous_artist)
    }

    pub fn scope_values(
        kind: SourceEntityKind,
        artist: &str,
        original: &crate::MetadataChanges,
        current: &crate::MetadataChanges,
    ) -> crate::MetadataChanges {
        tags::scope_values(kind, artist, original, current)
    }

    pub fn scope_metadata(
        kind: SourceEntityKind,
        artist: &str,
        original: &MetadataDraft,
        current: &crate::MetadataChanges,
    ) -> MetadataDraft {
        tags::scope_metadata(kind, artist, original, current)
    }

    pub fn can_embed_artist(format: Option<&str>) -> bool {
        format
            .and_then(crate::file::lofty::MetadataWriter::for_source_format)
            .is_some_and(|writer| writer.file_type() != lofty::file::FileType::Mp4)
    }
}

impl Source {
    async fn resolved_track(
        self: &Arc<Self>,
        database: &Database,
        uri: &str,
    ) -> Result<(NativeFile, library::TrackView), Error> {
        let row = database
            .track(&(uri), &ReadCancellation::new())
            .await
            .map_err(failure)?
            .ok_or(Error::Unavailable)?;
        let file = NativeFile::resolve(
            self,
            &row.media_uri,
            row.access.source_path.as_deref(),
            row.metadata.source_format.as_deref(),
            row.access.cue_path.is_some(),
            database.original_file(uri).await.map_err(failure)?,
        )?;
        Ok((file, row))
    }

    fn refresh_metadata_after_write(
        self: &Arc<Self>,
        database: &Database,
        mut work: Option<SaveWork>,
    ) {
        if !matches!(self.implementation, Implementation::OpenSubsonic(_)) {
            return;
        }
        let source = self.clone();
        let database = database.clone();
        tokio::spawn(async move {
            let result: Result<(), Error> = source
                .catalog_operation(&database, async |catalog| {
                    let Implementation::OpenSubsonic(provider) = &source.implementation else {
                        unreachable!()
                    };
                    provider.start_metadata_scan_and_wait(&|| source.is_retired()).await?;
                    if let Some(work) = &mut work {
                        let mut after = -1i64;
                        loop {
                            if source.is_retired() {
                                return Err(crate::SourceError::Cancelled.into());
                            }
                            let rows = sqlx::query("SELECT member,object_id FROM members WHERE member>?1 ORDER BY member LIMIT 128")
                                .bind(after).fetch_all(&mut work.0).await.map_err(failure)?;
                            if rows.is_empty() {
                                break;
                            }
                            after = rows.last().unwrap().get(0);
                            let ids = rows.into_iter().map(|row| source.item_request_id("track", row.get::<&str,_>(1))).collect();
                            source.acquire_items(catalog, ids, Vec::new()).await?;
                        }
                    }
                    Ok(())
                })
                .await;
            if let Err(error) = result {
                tracing::warn!(%error, "Could not refresh the server after writing file tags");
            }
        });
    }

    pub fn refresh_written_lyrics(self: &Arc<Self>, database: &Database) {
        self.refresh_metadata_after_write(database, None);
    }

    pub async fn write_file_track_rating(
        self: &Arc<Self>,
        database: &Database,
        source: library::SourceKey,
        uri: &str,
        rating: Option<u8>,
    ) -> Result<(), Error> {
        if !matches!(
            self.implementation,
            Implementation::Local(_) | Implementation::Files(_)
        ) {
            return Ok(());
        }
        self.catalog_operation(database, async |catalog| {
            let (file, row) = self.resolved_track(database, uri).await?;
            if row.source_key != Some(source) {
                return Err(Error::Unavailable);
            }
            let (prepared, _) = file
                .prepare(None)
                .await?
                .mutate(TagUpdate::Rating(rating), None)
                .await?;
            prepared
                .commit(&|| self.is_retired())
                .await?
                .write_catalog(self, catalog)
                .await
        })
        .await
    }

    pub async fn lyrics_storage(
        self: &Arc<Self>,
        database: &Database,
        uri: &str,
    ) -> Vec<crate::LyricsStorage> {
        let Ok(Some(row)) = database.track(&(uri), &ReadCancellation::new()).await else {
            return Vec::new();
        };
        if row.access.cue_path.is_some() {
            return Vec::new();
        }
        if let Implementation::JellyfinEmby(provider) = &self.implementation
            && provider.kind == crate::ServerKind::Jellyfin
        {
            return if row.object_id.is_some() {
                vec![crate::LyricsStorage::Server]
            } else {
                Vec::new()
            };
        }
        let Ok(original) = database.original_file(uri).await else {
            return Vec::new();
        };
        let Ok(file) = NativeFile::resolve(
            self,
            &row.media_uri,
            row.access.source_path.as_deref(),
            row.metadata.source_format.as_deref(),
            row.access.cue_path.is_some(),
            original,
        ) else {
            return Vec::new();
        };
        file.lyrics_storage()
    }

    pub async fn lyrics_writable(
        self: &Arc<Self>,
        database: &Database,
        uri: &str,
        sidecar: bool,
    ) -> bool {
        self.lyrics_storage(database, uri)
            .await
            .into_iter()
            .any(|storage| {
                storage == crate::LyricsStorage::Server
                    || storage
                        == if sidecar {
                            crate::LyricsStorage::Sidecar
                        } else {
                            crate::LyricsStorage::Embedded
                        }
            })
    }

    pub async fn write_lyrics(
        self: &Arc<Self>,
        database: &Database,
        uri: &str,
        lyrics: &str,
        sidecar: bool,
    ) -> Result<(), Error> {
        self.catalog_operation(database, async |catalog| {
            let row = database
                .track(&(uri), &ReadCancellation::new())
                .await
                .map_err(failure)?
                .ok_or(Error::Unavailable)?;
            if row.access.cue_path.is_some() {
                return Err(Error::Unavailable);
            }
            if let Implementation::JellyfinEmby(provider) = &self.implementation
                && provider.kind == crate::ServerKind::Jellyfin
            {
                if self.is_retired() {
                    return Err(crate::SourceError::Cancelled.into());
                }
                provider
                    .write_lyrics(row.object_id.as_deref().ok_or(Error::Unavailable)?, lyrics)
                    .await
                    .map_err(failure)?;
            } else {
                let file = NativeFile::resolve(
                    self,
                    &row.media_uri,
                    row.access.source_path.as_deref(),
                    row.metadata.source_format.as_deref(),
                    row.access.cue_path.is_some(),
                    database.original_file(uri).await.map_err(failure)?,
                )?;
                if let Some(committed) = file
                    .save_lyrics(lyrics, sidecar, || self.is_retired())
                    .await?
                {
                    committed.preserve_fingerprint(database).await?;
                    if matches!(
                        self.implementation,
                        Implementation::Local(_) | Implementation::Files(_)
                    ) {
                        committed.write_catalog(self, catalog).await?;
                    }
                }
            }
            if self.is_retired() {
                return Err(Error::SavedRefreshFailed(
                    crate::SourceError::Cancelled.to_string(),
                ));
            }
            database
                .remove_track_lyrics_by_authority(uri, "source")
                .await
                .map_err(|error| Error::SavedRefreshFailed(error.to_string()))?;
            Ok(())
        })
        .await
    }

    pub async fn write_file_r128_tags(
        self: &Arc<Self>,
        database: &Database,
        source: library::SourceKey,
        measurements: &[(library::TrackKey, Option<f64>, Option<f64>)],
    ) -> Result<(), Error> {
        self.catalog_operation(database, async |catalog| {
            self.write_r128_values(catalog, source, measurements).await
        })
        .await
    }

    async fn write_r128_values(
        self: &Arc<Self>,
        catalog: &mut library::Catalog<'_>,
        source: library::SourceKey,
        measurements: &[(library::TrackKey, Option<f64>, Option<f64>)],
    ) -> Result<(), Error> {
        let database = catalog.database;
        if !matches!(
            self.implementation,
            Implementation::Local(_) | Implementation::Files(_)
        ) {
            return Err(Error::Unavailable);
        }
        for page in measurements.chunks(64) {
            let rows = database
                .tracks(
                    &page.iter().map(|row| row.0).collect::<Vec<_>>(),
                    &ReadCancellation::new(),
                )
                .await
                .map_err(failure)?;
            let mut originals = database
                .original_files(
                    &rows
                        .iter()
                        .map(|row| row.media_uri.clone())
                        .collect::<Vec<_>>(),
                )
                .await
                .map_err(failure)?
                .into_iter()
                .map(|file| (file.media_uri.clone(), file))
                .collect::<std::collections::HashMap<_, _>>();
            for row in rows {
                if row.source_key != Some(source) || row.access.cue_path.is_some() {
                    continue;
                }
                let Some((_, track, album)) = page.iter().find(|value| value.0 == row.track_key)
                else {
                    continue;
                };
                let file = NativeFile::resolve(
                    self,
                    &row.media_uri,
                    row.access.source_path.as_deref(),
                    row.metadata.source_format.as_deref(),
                    row.access.cue_path.is_some(),
                    originals.remove(&row.media_uri),
                )?;
                if let Some(writer) = file
                    .format
                    .as_deref()
                    .and_then(crate::file::lofty::MetadataWriter::for_source_format)
                {
                    if track.is_some()
                        && !writer.metadata_key_is_writable(lofty::tag::ItemKey::R128TrackGain)
                        || album.is_some()
                            && !writer.metadata_key_is_writable(lofty::tag::ItemKey::R128AlbumGain)
                    {
                        continue;
                    }
                }
                let prepared = file.prepare(None).await?;
                let prepared = match prepared
                    .mutate(
                        TagUpdate::R128 {
                            track: *track,
                            album: *album,
                        },
                        None,
                    )
                    .await
                {
                    Ok((prepared, _)) => prepared,
                    Err(Error::Unavailable) => continue,
                    Err(error) => return Err(error),
                };
                let committed = prepared.commit(&|| self.is_retired()).await?;
                committed.write_catalog(self, catalog).await?;
            }
        }
        Ok(())
    }

    pub async fn backfill_file_r128_tags(
        self: &Arc<Self>,
        database: &Database,
        source: library::SourceKey,
    ) -> Result<(), Error> {
        self.catalog_operation(database, async |catalog| {
            if !matches!(
                self.implementation,
                Implementation::Local(_) | Implementation::Files(_)
            ) {
                return Err(Error::Unavailable);
            }
            let mut after = None;
            loop {
                let page = database
                    .r128_tag_write_page(source, after, 64, &ReadCancellation::new())
                    .await
                    .map_err(failure)?;
                if page.is_empty() {
                    return Ok(());
                }
                after = page.last().map(|row| row.track_key);
                let values = page
                    .into_iter()
                    .map(|row| (row.track_key, row.track_lufs, row.album_lufs))
                    .collect::<Vec<_>>();
                self.write_r128_values(catalog, source, &values).await?;
            }
        })
        .await
    }
}

fn write_native_bytes(path: &Path, bytes: &[u8], permissions_from: &Path) -> Result<(), Error> {
    use std::io::Write;
    let previous = match std::fs::metadata(path) {
        Ok(_) => Some(tags::revision(path)?),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(error) => return Err(failure(error)),
    };
    let parent = path.parent().ok_or(Error::Unavailable)?;
    let mut file = tempfile::NamedTempFile::new_in(parent).map_err(failure)?;
    file.write_all(bytes).map_err(failure)?;
    if let Ok(metadata) = std::fs::metadata(path).or_else(|_| std::fs::metadata(permissions_from)) {
        file.as_file()
            .set_permissions(metadata.permissions())
            .map_err(failure)?;
    }
    file.as_file().sync_all().map_err(failure)?;
    let current = match std::fs::metadata(path) {
        Ok(_) => Some(tags::revision(path)?),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(error) => return Err(failure(error)),
    };
    if current != previous {
        return Err(Error::Conflict);
    }
    let mut file = file.into_temp_path();
    if previous.is_some() {
        tags::replace(&mut file, path)?;
    } else {
        std::fs::rename(&file, path).map_err(failure)?;
    }
    tags::sync_parent(parent).map_err(|error| Error::PartiallySaved {
        reason: error.to_string(),
    })
}

impl NativeFile {
    async fn publish_direct(&self, database: &Database) -> Result<(), Error> {
        let FileLocation::Native(path) = &self.location else {
            return Err(Error::Unavailable);
        };
        if let Some(source_id) = database
            .track_source_by_uri(&self.media_uri, &ReadCancellation::new())
            .await
            .map_err(failure)?
        {
            let mut catalog = database
                .catalog(source_id.as_str(), None)
                .await
                .map_err(failure)?;
            let (observation, track) =
                crate::file::local::scan::read_saved_file(&catalog, &self.media_uri, path)
                    .await
                    .map_err(failure)?;
            let files = observation
                .map(|file| (file, Vec::new()))
                .into_iter()
                .collect::<Vec<_>>();
            catalog
                .write_file_observations(&files, vec![track], true)
                .await
                .map_err(failure)?;
            database
                .publish_catalog_update(source_id.as_str(), catalog.into_changes())
                .await;
        } else {
            let path = path.clone();
            let metadata = tokio::task::spawn_blocking(move || {
                crate::file::local::media::read_local_track_reference(&path)
            })
            .await
            .map_err(failure)?
            .ok_or_else(|| Error::Write("Saved media could not be read".into()))?
            .metadata;
            database
                .write_direct_track_metadata(&self.media_uri, &metadata)
                .await
                .map_err(failure)?;
        }
        Ok(())
    }

    fn folder_identity(&self, hint: Option<&str>) -> Result<String, Error> {
        Ok(match &self.location {
            FileLocation::Native(path) => hint
                .map(Path::new)
                .unwrap_or(path)
                .parent()
                .ok_or(Error::Unavailable)?
                .to_string_lossy()
                .into_owned(),
            FileLocation::Document { root, relative, .. } => format!(
                "{root}:{}",
                relative.rsplit_once('/').map_or("", |(parent, _)| parent)
            ),
            FileLocation::Remote { source, path } => {
                let files = source.file_source(Some(path)).ok_or(Error::Unavailable)?;
                let relative = files.relative(hint.unwrap_or(path)).map_err(failure)?;
                files
                    .location(
                        crate::file::documents::relative_parts(&relative)
                            .0
                            .rsplit_once('/')
                            .map_or("", |(parent, _)| parent),
                    )
                    .map_err(failure)?
            }
        })
    }

    pub(crate) async fn write_sidecar(&self, text: &str) -> Result<(), Error> {
        match &self.location {
            FileLocation::Native(audio) => {
                let audio = audio.clone();
                let text = text.to_owned();
                tokio::task::spawn_blocking(move || {
                    write_native_bytes(&audio.with_extension("lrc"), text.as_bytes(), &audio)
                })
                .await
                .map_err(failure)?
            }
            FileLocation::Document { root, relative, .. } => {
                let root = root.clone();
                let relative = relative.clone();
                let text = text.to_owned();
                tokio::task::spawn_blocking(move || {
                    let parent = relative.rsplit_once('/').map_or("", |(parent, _)| parent);
                    let directory = crate::file::documents::find_document(
                        &root,
                        &parent
                            .split('/')
                            .filter(|part| !part.is_empty())
                            .map(str::to_owned)
                            .collect::<Vec<_>>(),
                    )
                    .map_err(failure)?
                    .ok_or(Error::Unavailable)?;
                    let name = crate::file::remote::metadata::sidecar_path(
                        relative.rsplit('/').next().unwrap_or(&relative),
                    );
                    let current =
                        crate::file::documents::find_document_child(&directory.uri, &name)
                            .map_err(failure)?;
                    let file = tempfile::NamedTempFile::new().map_err(failure)?;
                    std::fs::write(file.path(), text).map_err(failure)?;
                    if let Some(current) = current {
                        crate::save_document(&current.uri, file.path(), current.revision.as_deref())
                            .map_err(failure)
                    } else {
                        crate::file::documents::create_document(
                            &directory.uri,
                            &name,
                            "text/plain",
                            file.path(),
                        )
                        .map(|_| ())
                        .map_err(failure)
                    }
                })
                .await
                .map_err(failure)?
            }
            FileLocation::Remote { source, path } => {
                let files = source.file_source(Some(path)).ok_or(Error::Unavailable)?;
                let relative = files.relative(path).map_err(failure)?;
                let relative = crate::file::remote::metadata::sidecar_path(
                    crate::file::documents::relative_parts(&relative).0,
                );
                let input = files.input().await.map_err(failure)?;
                let previous = match files.stat(&input, &relative).await {
                    Ok(file) => Some(file.revision),
                    Err(crate::SourceError::NotFound) => None,
                    Err(error) => return Err(failure(error)),
                };
                let file = tempfile::NamedTempFile::new().map_err(failure)?;
                tokio::fs::write(file.path(), text).await.map_err(failure)?;
                files
                    .commit_file(
                        &relative,
                        file.path(),
                        previous.as_ref().and_then(|value| value.as_deref()),
                        previous.is_some(),
                    )
                    .await
            }
        }
    }

    async fn write_folder_artwork(
        &self,
        _database: &Database,
        edit: &MetadataEdit,
        hint: Option<&str>,
        previous_artist: &str,
    ) -> Result<Vec<String>, Error> {
        let artwork = edit
            .artwork
            .as_ref()
            .filter(|art| art.storage == crate::ArtworkStorage::Folder)
            .cloned();
        let previous = (edit.kind == SourceEntityKind::Artist).then(|| previous_artist.to_owned());
        let next = tags::artist_name(edit, previous_artist).map(str::to_owned);
        match &self.location {
            FileLocation::Native(audio) => {
                let audio = audio.clone();
                let hint = hint.map(PathBuf::from);
                tokio::task::spawn_blocking(move || {
                    let parent = hint
                        .as_deref()
                        .unwrap_or(&audio)
                        .parent()
                        .ok_or(Error::Unavailable)?;
                    let current = hint.clone().or_else(|| {
                        previous.as_deref().map_or_else(
                            || crate::file::local::artwork::directory_image(parent),
                            |artist| {
                                crate::file::local::artwork::directory_artist_image_for(
                                    parent, artist, false,
                                )
                            },
                        )
                    });
                    let mut changed = Vec::new();
                    let Some(artwork) = artwork else {
                        if let (Some(current), Some(next)) = (current, next) {
                            let extension = current
                                .extension()
                                .and_then(|value| value.to_str())
                                .unwrap_or("jpg");
                            let destination = parent.join(tags::folder_artwork_filename(
                                current.file_name(),
                                Some(&next),
                                extension,
                            ));
                            if current != destination {
                                std::fs::rename(&current, &destination).map_err(failure)?;
                                changed.extend([
                                    current.to_string_lossy().into_owned(),
                                    destination.to_string_lossy().into_owned(),
                                ]);
                                tags::sync_parent(parent)?;
                            }
                        }
                        return Ok(changed);
                    };
                    if let crate::ArtworkChange::Replace(image) = artwork.change {
                        let destination = parent.join(tags::folder_artwork_filename(
                            current.as_deref().and_then(Path::file_name),
                            next.as_deref(),
                            tags::artwork_extension(&image)?,
                        ));
                        write_native_bytes(&destination, &image.bytes, &audio)?;
                        changed.push(destination.to_string_lossy().into_owned());
                        if let Some(current) = current.filter(|current| *current != destination) {
                            std::fs::remove_file(&current).map_err(|error| {
                                Error::PartiallySaved {
                                    reason: error.to_string(),
                                }
                            })?;
                            changed.push(current.to_string_lossy().into_owned());
                        }
                    } else if let Some(current) = current {
                        std::fs::remove_file(&current).map_err(failure)?;
                        changed.push(current.to_string_lossy().into_owned());
                    }
                    tags::sync_parent(parent)?;
                    Ok(changed)
                })
                .await
                .map_err(failure)?
            }
            FileLocation::Document {
                uri,
                root,
                relative,
            } => {
                let locator = library::LocalLocatorWrite {
                    source_id: None,
                    media_uri: self.media_uri.clone(),
                    origin: "mapping".into(),
                    path: uri.clone(),
                    root: root.clone(),
                    relative_path: relative.clone(),
                    access_uri: uri.clone(),
                };
                tokio::task::spawn_blocking(move || {
                    crate::file::documents::save_mapped_artwork(
                        &locator,
                        artwork.as_ref(),
                        previous.as_deref(),
                        next.as_deref(),
                    )
                    .map(|_| Vec::new())
                })
                .await
                .map_err(failure)?
            }
            FileLocation::Remote { source, path } => {
                let files = source.file_source(Some(path)).ok_or(Error::Unavailable)?;
                let relative = files.relative(hint.unwrap_or(path)).map_err(failure)?;
                let directory = crate::file::documents::relative_parts(&relative)
                    .0
                    .rsplit_once('/')
                    .map_or("", |(parent, _)| parent);
                let mut best = None;
                let mut count = 0;
                if hint.is_some() {
                    best = Some((0usize, relative.clone()));
                } else {
                    files
                        .visit_files(directory, "", |(path, _)| {
                            let display = crate::file::documents::relative_parts(&path).0;
                            let name = display.rsplit('/').next().unwrap_or(display);
                            if crate::file::artwork::supported_image(Path::new(name))
                                && (previous.is_some()
                                    || !crate::file::artwork::is_artist_image(name))
                            {
                                count += 1;
                                let rank = previous.as_deref().map_or_else(
                                    || crate::file::artwork::image_rank(name),
                                    |artist| {
                                        crate::file::artwork::artist_image_rank_for(
                                            name, artist, false,
                                        )
                                    },
                                );
                                let candidate = (rank, path);
                                if best.as_ref().is_none_or(|best| &candidate < best) {
                                    best = Some(candidate);
                                }
                            }
                            std::future::ready(Ok(()))
                        })
                        .await
                        .map_err(failure)?;
                }
                let current = best
                    .filter(|(rank, _)| *rank != usize::MAX || previous.is_none() && count == 1)
                    .map(|(_, path)| path);
                let input = files.input().await.map_err(failure)?;
                let artwork = if let Some(artwork) = artwork {
                    artwork
                } else if let Some(current) = &current {
                    let bytes = files
                        .small_file(&input, current, 32 * 1024 * 1024)
                        .await
                        .map_err(failure)?;
                    crate::ArtworkEdit {
                        storage: crate::ArtworkStorage::Folder,
                        change: crate::ArtworkChange::Replace(Arc::new(crate::ImageBytes {
                            bytes,
                            content_type: crate::file::artwork::content_type(Path::new(
                                crate::file::documents::relative_parts(current).0,
                            )),
                        })),
                    }
                } else {
                    return Ok(Vec::new());
                };
                let mut changed = Vec::new();
                if let crate::ArtworkChange::Replace(image) = artwork.change {
                    let name = current
                        .as_deref()
                        .map(crate::file::documents::relative_parts)
                        .map(|(path, _)| Path::new(path))
                        .and_then(Path::file_name);
                    let filename = tags::folder_artwork_filename(
                        name,
                        next.as_deref(),
                        tags::artwork_extension(&image)?,
                    );
                    let destination = if directory.is_empty() {
                        filename.to_string_lossy().into_owned()
                    } else {
                        format!("{directory}/{}", filename.to_string_lossy())
                    };
                    let destination = current
                        .as_ref()
                        .filter(|current| {
                            crate::file::documents::relative_parts(current).0 == destination
                        })
                        .cloned()
                        .unwrap_or(destination);
                    let previous = match files.stat(&input, &destination).await {
                        Ok(file) => Some(file.revision),
                        Err(crate::SourceError::NotFound) => None,
                        Err(error) => return Err(failure(error)),
                    };
                    let file = tempfile::NamedTempFile::new().map_err(failure)?;
                    tokio::fs::write(file.path(), &image.bytes)
                        .await
                        .map_err(failure)?;
                    files
                        .commit_file(
                            &destination,
                            file.path(),
                            previous.as_ref().and_then(|value| value.as_deref()),
                            previous.is_some(),
                        )
                        .await?;
                    changed.push(
                        files
                            .stat(&input, &destination)
                            .await
                            .map_err(|error| Error::SavedRefreshFailed(error.to_string()))?
                            .path,
                    );
                    if let Some(current) = current.filter(|current| *current != destination) {
                        files.remove_file(&current).await.map_err(|error| {
                            Error::PartiallySaved {
                                reason: error.to_string(),
                            }
                        })?;
                        changed.push(files.location(&current).map_err(failure)?);
                    }
                } else if let Some(current) = current {
                    files.remove_file(&current).await.map_err(failure)?;
                    changed.push(files.location(&current).map_err(failure)?);
                }
                Ok(changed)
            }
        }
    }
}
