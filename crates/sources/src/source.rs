use std::collections::BTreeSet;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use async_channel::Sender;
use library::{
    CandidateBatch, CandidateFinish, CandidateHeader, FavoriteAcceptance, FavoriteItemId,
    FolderContents, FolderId, HomeFacts, HomeSectionKind, ImageRef, Libraries, Library,
    LibraryError, LocalAccessTarget, LocalArtworkRef, LocalComponentReplacement, MetadataDraft,
    MetadataEdit, MetadataError, MetadataValues, MusicFolderId, PlaylistAcceptance, PlaylistEdit,
    PlaylistId, PlaylistSnapshot, PreparedSourceCandidate, ProviderFreshness, RadioSeed,
    RandomCriteria, ResolvedStream, SearchRequest, SearchResults, SourceHomeSection,
    SourceHomeSectionKind, SourceId, StreamRequest, TrackId,
};
use playback::SourceReportFact;

use crate::{
    ImageBytes, LyricsSearch, NativeLyrics, SourceConfiguration, SourceError, SourceResult,
    SourceSettingsInput, SourceSetupInput,
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SourceReadStage {
    Albums,
    Tracks,
    Artists,
    Genres,
    Playlists,
    Home,
    Artwork,
    Files,
    Finalizing,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SourceReadProgress {
    pub stage: SourceReadStage,
    pub completed: usize,
    pub total: Option<usize>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum NativeSourceResult<T> {
    Available(T),
    Unavailable,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ResolvedDownload {
    stream: ResolvedStream,
    transcoded_extension: Option<&'static str>,
}

impl ResolvedDownload {
    pub(crate) fn new(stream: ResolvedStream, transcoded_extension: Option<&'static str>) -> Self {
        Self {
            stream,
            transcoded_extension,
        }
    }

    pub fn stream(&self) -> &ResolvedStream {
        &self.stream
    }

    pub fn into_stream(self) -> ResolvedStream {
        self.stream
    }

    pub fn transcoded_extension(&self) -> Option<&'static str> {
        self.transcoded_extension
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SourceImageRequest {
    Native { image_ref: ImageRef, size: u32 },
    Local(LocalArtworkRef),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SourceFreshness {
    Unavailable,
    Unchanged,
    Busy,
    Changed(ProviderFreshness),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ObservedSourceChange {
    Full,
    Jellyfin {
        upserts: BTreeSet<String>,
        removals: BTreeSet<String>,
    },
    LocalPaths(BTreeSet<PathBuf>),
    LocalRescan,
}

impl ObservedSourceChange {
    pub fn merge(&mut self, other: Self) -> Result<(), Self> {
        match (&mut *self, other) {
            (Self::Full, Self::Full | Self::Jellyfin { .. }) => {}
            (current @ Self::Jellyfin { .. }, Self::Full) => {
                *current = Self::Full;
            }
            (
                Self::Jellyfin { upserts, removals },
                Self::Jellyfin {
                    upserts: incoming_upserts,
                    removals: incoming_removals,
                },
            ) => {
                upserts.retain(|id| !incoming_removals.contains(id));
                removals.retain(|id| !incoming_upserts.contains(id));
                upserts.extend(incoming_upserts);
                removals.extend(incoming_removals);
            }
            (Self::LocalRescan, Self::LocalRescan | Self::LocalPaths(_)) => {}
            (current @ Self::LocalPaths(_), Self::LocalRescan) => {
                *current = Self::LocalRescan;
            }
            (Self::LocalPaths(current), Self::LocalPaths(incoming)) => current.extend(incoming),
            (_, incoming) => return Err(incoming),
        }
        Ok(())
    }

    pub(crate) fn jellyfin(upserts: BTreeSet<String>, removals: BTreeSet<String>) -> Self {
        Self::Jellyfin { upserts, removals }
    }

    pub fn full() -> Self {
        Self::Full
    }
}

#[derive(Debug)]
pub enum PreparedSourceChange {
    SourceUpdate(library::SourceLibraryUpdate),
    LocalReplacement(LocalComponentReplacement),
    Full,
    Ignored,
}

pub struct ConnectedSource {
    configuration: SourceConfiguration,
    source: Source,
    credential: Option<String>,
}

pub enum SourceEditResult {
    Unchanged,
    ConfigurationOnly(SourceConfiguration),
    Connected(Box<ConnectedSource>),
}

pub(crate) trait RemotePlaylistSource {
    async fn create_playlist(&self, name: &str, track_ids: &[TrackId]) -> SourceResult<PlaylistId>;
    async fn rename_playlist(&self, playlist_id: &PlaylistId, name: &str) -> SourceResult<()>;
    async fn delete_playlist(&self, playlist_id: &PlaylistId) -> SourceResult<()>;
    async fn add_playlist_tracks(
        &self,
        playlist_id: &PlaylistId,
        track_ids: &[TrackId],
    ) -> SourceResult<()>;
    async fn remove_playlist_entries(
        &self,
        playlist_id: &PlaylistId,
        occurrence_ids: &[String],
    ) -> SourceResult<()>;
    async fn move_playlist_entry(
        &self,
        playlist_id: &PlaylistId,
        occurrence_id: &str,
        new_index: usize,
    ) -> SourceResult<()>;
    async fn read_playlist_snapshot(
        &self,
        playlist_id: &PlaylistId,
    ) -> SourceResult<PlaylistSnapshot>;
}

impl ConnectedSource {
    pub fn into_parts(self) -> (SourceConfiguration, Source, Option<String>) {
        (self.configuration, self.source, self.credential)
    }

    pub(crate) fn local(
        configuration: SourceConfiguration,
        source: crate::local::LocalSource,
    ) -> Self {
        Self {
            source: Source::new(
                configuration.source_id.clone(),
                Implementation::Local(source),
            ),
            configuration,
            credential: None,
        }
    }

    pub(crate) fn jellyfin(
        configuration: SourceConfiguration,
        source: crate::jellyfin::JellyfinSource,
        credential: Option<String>,
    ) -> Self {
        Self {
            source: Source::new(
                configuration.source_id.clone(),
                Implementation::Jellyfin(source),
            ),
            configuration,
            credential,
        }
    }

    pub(crate) fn subsonic(
        configuration: SourceConfiguration,
        source: crate::subsonic::SubsonicSource,
        credential: Option<String>,
    ) -> Self {
        Self {
            source: Source::new(
                configuration.source_id.clone(),
                Implementation::OpenSubsonic(source),
            ),
            configuration,
            credential,
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum SourceChangePreparationError {
    #[error(transparent)]
    Source(#[from] SourceError),
    #[error(transparent)]
    Library(#[from] library::LibraryError),
    #[error("source change preparation task failed: {0}")]
    Task(#[from] tokio::task::JoinError),
}

#[derive(Debug, thiserror::Error)]
pub enum SourceCandidatePreparationError {
    #[error(transparent)]
    Source(#[from] SourceError),
    #[error(transparent)]
    Library(#[from] LibraryError),
    #[error("source candidate task failed: {0}")]
    Task(#[from] tokio::task::JoinError),
}

/// Inputs that determine the canonical facts emitted by one source.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SourceInputIdentity {
    pub source_id: SourceId,
    pub digest: [u8; 32],
}

/// Direct bounded edge from a provider response to Library's candidate.
pub(crate) struct BatchEmitter {
    batches: Sender<CandidateBatch>,
}

impl BatchEmitter {
    pub(crate) fn new(batches: Sender<CandidateBatch>) -> Self {
        Self { batches }
    }

    pub(crate) fn emit(&self, batch: CandidateBatch) -> SourceResult<()> {
        if batch.is_empty() {
            return Ok(());
        }
        self.batches
            .send_blocking(batch)
            .map_err(|_| SourceError::Cancelled)
    }

    pub(crate) async fn emit_async(&self, batch: CandidateBatch) -> SourceResult<()> {
        if batch.is_empty() {
            return Ok(());
        }
        self.batches
            .send(batch)
            .await
            .map_err(|_| SourceError::Cancelled)
    }
}

/// One opened concrete source. Its provider enum remains private.
pub struct Source {
    source_id: SourceId,
    implementation: Implementation,
}

enum Implementation {
    Local(crate::local::LocalSource),
    Jellyfin(crate::jellyfin::JellyfinSource),
    OpenSubsonic(crate::subsonic::SubsonicSource),
}

impl Source {
    fn new(source_id: SourceId, implementation: Implementation) -> Self {
        Self {
            source_id,
            implementation,
        }
    }

    pub async fn connect(input: SourceSetupInput) -> SourceResult<ConnectedSource> {
        match input {
            SourceSetupInput::Local(input) => crate::local::connect(input),
            SourceSetupInput::Jellyfin(input) => crate::jellyfin::connect(input).await,
            SourceSetupInput::Subsonic {
                flavor,
                authentication,
                credentials,
            } => crate::subsonic::connect(flavor, authentication, credentials).await,
        }
    }

    /// Apply an edit to one configured source without exposing provider payloads
    /// outside Sources.
    ///
    /// The returned configuration carries the provider's canonical account
    /// identity. Retaining the current SourceId means the account is unchanged;
    /// a new SourceId means Rufin must replace the configured account.
    pub async fn edit(
        current: SourceConfiguration,
        current_credential: Option<String>,
        input: SourceSettingsInput,
        jellyfin_device_id: Option<String>,
    ) -> SourceResult<SourceEditResult> {
        match input {
            SourceSettingsInput::Local { roots } => crate::local::edit(current, roots),
            SourceSettingsInput::Jellyfin(input) => {
                crate::jellyfin::edit(current, current_credential, input, jellyfin_device_id).await
            }
            SourceSettingsInput::Subsonic {
                authentication,
                credentials,
            } => {
                crate::subsonic::edit(current, current_credential, authentication, credentials)
                    .await
            }
        }
    }

    /// Open a configured source without contacting it.
    ///
    /// Cache usability and source access are decided later. In particular, a
    /// temporarily unavailable Local root does not erase or invalidate its
    /// saved source.
    pub fn open(
        configuration: SourceConfiguration,
        credential: Option<String>,
        jellyfin_device_id: Option<String>,
    ) -> SourceResult<Self> {
        let implementation =
            match configuration.kind.as_str() {
                crate::local::LOCAL_SOURCE_ID => Implementation::Local(
                    crate::local::LocalSource::from_configuration(&configuration)?,
                ),
                crate::jellyfin::JELLYFIN_SOURCE_ID => Implementation::Jellyfin(
                    crate::jellyfin::open(&configuration, credential, jellyfin_device_id)?,
                ),
                "navidrome" | "subsonic" => {
                    Implementation::OpenSubsonic(crate::subsonic::open(&configuration, credential)?)
                }
                kind => {
                    return Err(SourceError::InvalidConfig(format!(
                        "unknown source kind {kind}"
                    )));
                }
            };
        Ok(Self::new(configuration.source_id, implementation))
    }

    pub fn source_id(&self) -> &SourceId {
        &self.source_id
    }

    pub fn metadata_editing_available(&self, item: &library::MetadataItem) -> bool {
        match &self.implementation {
            Implementation::Local(local) => local.metadata_entry_available(item),
            Implementation::Jellyfin(source) => source.metadata_entry_available(item),
            Implementation::OpenSubsonic(source) => {
                source.metadata_editing_available()
                    && crate::local::mapped_metadata_editing_available(item)
            }
        }
    }

    pub async fn read_metadata(
        self: &Arc<Self>,
        subject: library::MetadataSubject,
        local_access: Option<Vec<LocalAccessTarget>>,
    ) -> Result<MetadataDraft, MetadataError> {
        match &self.implementation {
            Implementation::Local(_) => {
                let source = Arc::clone(self);
                tokio::task::spawn_blocking(move || {
                    let Implementation::Local(local) = &source.implementation else {
                        unreachable!("source implementation changed")
                    };
                    local.read_metadata(&subject)
                })
                .await
                .map_err(|error| MetadataError::Write(error.to_string()))?
            }
            Implementation::Jellyfin(source) => {
                let editing = source
                    .metadata_editing(subject.item())
                    .await
                    .ok_or(MetadataError::Unavailable)?;
                source
                    .read_metadata(subject.item(), editing)
                    .await
                    .map_err(|error| MetadataError::Write(error.to_string()))
            }
            Implementation::OpenSubsonic(source) => {
                if !source.metadata_editing_available() {
                    return Err(MetadataError::Unavailable);
                }
                let local_access =
                    local_access.ok_or_else(|| MetadataError::LocalAccessRequired {
                        source_path: String::new(),
                    })?;
                tokio::task::spawn_blocking(move || {
                    crate::local::read_mapped_metadata(&subject, &local_access)
                })
                .await
                .map_err(|error| MetadataError::Write(error.to_string()))?
            }
        }
    }

    pub async fn identify_metadata(
        &self,
        subject: &library::MetadataSubject,
        values: &MetadataValues,
    ) -> Result<Option<library::MetadataIdentification>, String> {
        match &self.implementation {
            Implementation::Jellyfin(source) => {
                source.identify_metadata(subject.item(), values).await
            }
            Implementation::Local(_) | Implementation::OpenSubsonic(_) => Ok(None),
        }
    }

    pub fn metadata_source_search(&self, subject: &library::MetadataSubject) -> bool {
        match &self.implementation {
            Implementation::Jellyfin(source) => source.metadata_source_search(subject.item()),
            Implementation::Local(_) | Implementation::OpenSubsonic(_) => false,
        }
    }

    pub fn needs_metadata_local_access(&self) -> bool {
        match &self.implementation {
            Implementation::OpenSubsonic(source) => source.metadata_editing_available(),
            Implementation::Local(_) | Implementation::Jellyfin(_) => false,
        }
    }

    pub async fn write_metadata(
        self: &Arc<Self>,
        library: Arc<Library>,
        subject: library::MetadataSubject,
        edit: MetadataEdit,
        local_access: Option<Vec<LocalAccessTarget>>,
        progress: Arc<dyn Fn(SourceReadProgress) + Send + Sync>,
        cancelled: Arc<AtomicBool>,
    ) -> Result<PreparedSourceChange, MetadataError> {
        let (change, ignored_message) = match &self.implementation {
            Implementation::Local(_) => {
                let source = Arc::clone(self);
                let paths = tokio::task::spawn_blocking(move || {
                    let Implementation::Local(local) = &source.implementation else {
                        unreachable!("source implementation changed")
                    };
                    local.write_metadata(&subject, &edit)
                })
                .await
                .map_err(|error| MetadataError::Write(error.to_string()))??;
                (
                    ObservedSourceChange::LocalPaths(paths),
                    "the written files were not accepted by the Local source",
                )
            }
            Implementation::Jellyfin(source) => {
                let raw_ids = source.write_metadata(subject.item(), &edit).await?;
                (
                    ObservedSourceChange::jellyfin(raw_ids.into_iter().collect(), BTreeSet::new()),
                    "the source did not return the written item",
                )
            }
            Implementation::OpenSubsonic(source) => {
                if !source.metadata_editing_available() {
                    return Err(MetadataError::Unavailable);
                }
                let local_access =
                    local_access.ok_or_else(|| MetadataError::LocalAccessRequired {
                        source_path: String::new(),
                    })?;
                source
                    .require_metadata_scan_idle()
                    .await
                    .map_err(|error| MetadataError::Write(error.to_string()))?;
                tokio::task::spawn_blocking(move || {
                    crate::local::write_mapped_metadata(&subject, &local_access, &edit)
                })
                .await
                .map_err(|error| MetadataError::Write(error.to_string()))??;
                source
                    .start_metadata_scan_and_wait()
                    .await
                    .map_err(|error| MetadataError::SavedRefreshFailed(error.to_string()))?;
                (
                    ObservedSourceChange::Full,
                    "the source did not return the written item",
                )
            }
        };
        let prepared = self
            .prepare_change(library, change, progress, cancelled)
            .await
            .map_err(|error| MetadataError::SavedRefreshFailed(error.to_string()))?;
        match prepared {
            PreparedSourceChange::Ignored => Err(MetadataError::SavedRefreshFailed(
                ignored_message.to_string(),
            )),
            prepared => Ok(prepared),
        }
    }

    pub fn listen_local_changes(
        &self,
        catch_up: bool,
        on_change: impl FnMut(ObservedSourceChange) -> bool,
        should_stop: impl Fn() -> bool,
    ) -> SourceResult<()> {
        let Implementation::Local(source) = &self.implementation else {
            return Err(SourceError::InvalidRequest(
                "the Local change feed belongs to another source",
            ));
        };
        let on_change = std::cell::RefCell::new(on_change);
        let mut on_ready = |reconnecting| {
            let request = catch_up || reconnecting;
            !request || on_change.borrow_mut()(ObservedSourceChange::LocalRescan)
        };
        let mut on_change = |change| on_change.borrow_mut()(change);
        source.watch(&mut on_ready, &mut on_change, &should_stop)
    }

    pub async fn listen_jellyfin_changes(
        self: Arc<Self>,
        mut on_ready: impl FnMut() -> bool + Send + 'static,
        mut on_gap: impl FnMut() -> bool + Send + 'static,
        mut on_change: impl FnMut(ObservedSourceChange) -> bool + Send + 'static,
    ) -> SourceResult<()> {
        let Implementation::Jellyfin(source) = &self.implementation else {
            return Err(SourceError::InvalidRequest(
                "the change feed belongs to another source",
            ));
        };
        source.refresh_metadata_editing().await;
        source
            .listen_library_changes(&mut on_ready, &mut on_gap, &mut on_change)
            .await
    }

    pub async fn prepare_change(
        self: &Arc<Self>,
        library: Arc<Library>,
        change: ObservedSourceChange,
        progress: Arc<dyn Fn(SourceReadProgress) + Send + Sync>,
        cancelled: Arc<AtomicBool>,
    ) -> Result<PreparedSourceChange, SourceChangePreparationError> {
        match change {
            ObservedSourceChange::Full => Ok(PreparedSourceChange::Full),
            ObservedSourceChange::Jellyfin { upserts, removals } => {
                let Implementation::Jellyfin(source) = &self.implementation else {
                    return Err(SourceError::InvalidRequest(
                        "the library change belongs to another source",
                    )
                    .into());
                };
                source
                    .prepare_change(&library, upserts, removals)
                    .await
                    .map_err(Into::into)
            }
            change @ (ObservedSourceChange::LocalPaths(_) | ObservedSourceChange::LocalRescan) => {
                let source = Arc::clone(self);
                tokio::task::spawn_blocking(move || {
                    let Implementation::Local(local) = &source.implementation else {
                        return Err(SourceError::InvalidRequest(
                            "filesystem change preparation requires a Local source",
                        )
                        .into());
                    };
                    let should_stop = || cancelled.load(Ordering::Acquire);
                    local
                        .prepare_change(
                            &library,
                            change,
                            unix_seconds(),
                            progress.as_ref(),
                            &should_stop,
                        )
                        .map(|replacement| {
                            replacement.map_or(
                                PreparedSourceChange::Ignored,
                                PreparedSourceChange::LocalReplacement,
                            )
                        })
                })
                .await?
            }
        }
    }

    pub fn inspect_accepted_artwork(
        &self,
        library: &Library,
        cancelled: &(dyn Fn() -> bool + Send + Sync),
    ) -> Result<Option<LocalComponentReplacement>, SourceChangePreparationError> {
        let Implementation::Local(local) = &self.implementation else {
            return Ok(None);
        };
        local.inspect_accepted_artwork(library, unix_seconds(), cancelled)
    }

    pub async fn check_freshness(
        &self,
        accepted: Option<&ProviderFreshness>,
    ) -> SourceResult<SourceFreshness> {
        match &self.implementation {
            Implementation::OpenSubsonic(source) => source.check_freshness(accepted).await,
            Implementation::Local(_) | Implementation::Jellyfin(_) => {
                Ok(SourceFreshness::Unavailable)
            }
        }
    }

    pub async fn folder(
        &self,
        folder_id: Option<&FolderId>,
        music_folder_id: Option<&MusicFolderId>,
    ) -> SourceResult<NativeSourceResult<FolderContents>> {
        match &self.implementation {
            Implementation::Local(_) => Ok(NativeSourceResult::Unavailable),
            Implementation::Jellyfin(source) => source
                .read_folder(folder_id, music_folder_id)
                .await
                .map(NativeSourceResult::Available),
            Implementation::OpenSubsonic(source) => source
                .read_folder(folder_id, music_folder_id)
                .await
                .map(NativeSourceResult::Available),
        }
    }

    pub async fn search(
        &self,
        request: &SearchRequest,
    ) -> SourceResult<NativeSourceResult<SearchResults>> {
        match &self.implementation {
            Implementation::Local(_) => Ok(NativeSourceResult::Unavailable),
            Implementation::Jellyfin(source) => source
                .search(request)
                .await
                .map(NativeSourceResult::Available),
            Implementation::OpenSubsonic(source) => source
                .search(request)
                .await
                .map(NativeSourceResult::Available),
        }
    }

    /// Reads one provider-owned Home section.
    ///
    /// Local and Explore are composed from the accepted Library instead.
    pub async fn home_section(
        &self,
        kind: HomeSectionKind,
    ) -> SourceResult<NativeSourceResult<SourceHomeSection>> {
        let kind = match kind {
            HomeSectionKind::Explore => return Ok(NativeSourceResult::Unavailable),
            HomeSectionKind::MostPlayed => SourceHomeSectionKind::MostPlayed,
            HomeSectionKind::NewlyAdded => SourceHomeSectionKind::NewlyAdded,
            HomeSectionKind::RecentlyPlayed => SourceHomeSectionKind::RecentlyPlayed,
            HomeSectionKind::RecentlyReleased => SourceHomeSectionKind::RecentlyReleased,
        };
        match &self.implementation {
            Implementation::Local(_) => Ok(NativeSourceResult::Unavailable),
            Implementation::Jellyfin(source) => source
                .read_home_section(kind)
                .await
                .map(NativeSourceResult::Available),
            Implementation::OpenSubsonic(source) => source
                .read_home_section(kind)
                .await
                .map(NativeSourceResult::Available),
        }
    }

    pub async fn random_tracks(
        &self,
        criteria: &RandomCriteria,
    ) -> SourceResult<NativeSourceResult<Vec<library::Track>>> {
        match &self.implementation {
            Implementation::Local(_) => Ok(NativeSourceResult::Unavailable),
            Implementation::Jellyfin(source) => source
                .random_tracks(criteria)
                .await
                .map(NativeSourceResult::Available),
            Implementation::OpenSubsonic(source) => source
                .random_tracks(criteria)
                .await
                .map(NativeSourceResult::Available),
        }
    }

    pub async fn generated_tracks(
        &self,
        seed: &RadioSeed,
        limit: usize,
    ) -> SourceResult<NativeSourceResult<Vec<library::Track>>> {
        match &self.implementation {
            Implementation::Local(_) => Ok(NativeSourceResult::Unavailable),
            Implementation::Jellyfin(source) => source
                .generated_tracks(seed, limit)
                .await
                .map(NativeSourceResult::Available),
            Implementation::OpenSubsonic(source) => source
                .generated_tracks(seed, limit)
                .await
                .map(NativeSourceResult::Available),
        }
    }

    pub async fn stream(
        &self,
        request: &StreamRequest,
    ) -> SourceResult<NativeSourceResult<ResolvedStream>> {
        match &self.implementation {
            Implementation::Local(_) => Ok(NativeSourceResult::Unavailable),
            Implementation::Jellyfin(source) => source
                .resolve_stream(request)
                .await
                .map(NativeSourceResult::Available),
            Implementation::OpenSubsonic(source) => source
                .resolve_stream(request)
                .await
                .map(NativeSourceResult::Available),
        }
    }

    pub fn resolve_download(
        &self,
        request: &StreamRequest,
    ) -> SourceResult<NativeSourceResult<ResolvedDownload>> {
        match &self.implementation {
            Implementation::Local(_) => Ok(NativeSourceResult::Unavailable),
            Implementation::Jellyfin(source) => source
                .resolve_download(request)
                .map(NativeSourceResult::Available),
            Implementation::OpenSubsonic(source) => source
                .resolve_download(request)
                .map(NativeSourceResult::Available),
        }
    }

    pub async fn image(&self, request: SourceImageRequest) -> SourceResult<ImageBytes> {
        match (&self.implementation, request) {
            (Implementation::Local(source), SourceImageRequest::Local(reference)) => {
                source.image_bytes(&reference)
            }
            (Implementation::Jellyfin(source), SourceImageRequest::Native { image_ref, size }) => {
                source.image_bytes(&image_ref, size).await
            }
            (
                Implementation::OpenSubsonic(source),
                SourceImageRequest::Native { image_ref, size },
            ) => source.image_bytes(&image_ref, size).await,
            _ => Err(SourceError::InvalidRequest(
                "artwork does not belong to the selected source",
            )),
        }
    }

    pub async fn set_favorite(
        &self,
        item: FavoriteItemId,
        favorite: bool,
    ) -> SourceResult<FavoriteAcceptance> {
        match &self.implementation {
            Implementation::Local(_) => Ok(FavoriteAcceptance::RufinOwned { item, favorite }),
            Implementation::Jellyfin(source) => {
                source.set_favorite(item.clone(), favorite).await?;
                Ok(FavoriteAcceptance::SourceAcknowledged { item, favorite })
            }
            Implementation::OpenSubsonic(source) => {
                source.set_favorite(item.clone(), favorite).await?;
                Ok(FavoriteAcceptance::SourceAcknowledged { item, favorite })
            }
        }
    }

    pub async fn set_rating(
        &self,
        item: FavoriteItemId,
        rating: Option<u8>,
    ) -> SourceResult<NativeSourceResult<()>> {
        match &self.implementation {
            Implementation::Local(_) => Ok(NativeSourceResult::Unavailable),
            Implementation::Jellyfin(source) => source
                .set_rating(item, rating)
                .await
                .map(NativeSourceResult::Available),
            Implementation::OpenSubsonic(source) => source
                .set_rating(item, rating)
                .await
                .map(NativeSourceResult::Available),
        }
    }

    pub fn set_local_rating(
        &self,
        track: &library::Track,
        rating: Option<u8>,
    ) -> SourceResult<NativeSourceResult<()>> {
        match &self.implementation {
            Implementation::Local(source) => source
                .write_rating(track, rating)
                .map(|written| {
                    if written {
                        NativeSourceResult::Available(())
                    } else {
                        NativeSourceResult::Unavailable
                    }
                })
                .map_err(|error| SourceError::Other(error.to_string())),
            Implementation::Jellyfin(_) | Implementation::OpenSubsonic(_) => {
                Ok(NativeSourceResult::Unavailable)
            }
        }
    }

    pub async fn edit_playlist(&self, edit: PlaylistEdit) -> SourceResult<PlaylistAcceptance> {
        match &self.implementation {
            Implementation::Local(_) => Ok(PlaylistAcceptance::RufinOwned(edit)),
            Implementation::Jellyfin(source) => edit_remote_playlist(source, edit).await,
            Implementation::OpenSubsonic(source) => edit_remote_playlist(source, edit).await,
        }
    }

    pub async fn lyrics(
        &self,
        track_id: &TrackId,
        search: LyricsSearch,
    ) -> SourceResult<NativeSourceResult<Option<NativeLyrics>>> {
        match &self.implementation {
            Implementation::Local(_) => Ok(NativeSourceResult::Unavailable),
            Implementation::Jellyfin(source) => source
                .lyrics(track_id, search)
                .await
                .map(NativeSourceResult::Available),
            Implementation::OpenSubsonic(source) => source
                .lyrics(track_id, search)
                .await
                .map(NativeSourceResult::Available),
        }
    }

    pub async fn report_playback(
        &self,
        report: SourceReportFact,
    ) -> SourceResult<NativeSourceResult<()>> {
        match &self.implementation {
            Implementation::Local(_) => Ok(NativeSourceResult::Unavailable),
            Implementation::Jellyfin(source) => source
                .report_playback(report)
                .await
                .map(NativeSourceResult::Available),
            Implementation::OpenSubsonic(source) => source
                .report_playback(report)
                .await
                .map(NativeSourceResult::Available),
        }
    }

    /// Reads one complete source snapshot into an invisible Library candidate.
    ///
    /// The bounded writer keeps provider acquisition backpressured. Library
    /// remains the only owner of persistence, comparison, and later acceptance.
    pub async fn prepare_library_candidate(
        self: Arc<Self>,
        libraries: Libraries,
        identity: SourceInputIdentity,
        current: Option<Arc<Library>>,
        progress: Arc<dyn Fn(SourceReadProgress) + Send + Sync>,
        cancelled: Arc<AtomicBool>,
    ) -> Result<PreparedSourceCandidate, SourceCandidatePreparationError> {
        let header = CandidateHeader {
            source_id: identity.source_id,
            input_digest: identity.digest,
        };
        let (batches, receiver) = async_channel::bounded(1);
        let writer = tokio::task::spawn_blocking(move || {
            let mut candidate = libraries.begin_source_candidate(header)?;
            while let Ok(batch) = receiver.recv_blocking() {
                candidate.write(batch)?;
            }
            Ok::<_, LibraryError>(candidate)
        });

        let facts = Arc::clone(&self)
            .read_source_facts(batches, progress, Arc::clone(&cancelled))
            .await;
        let candidate = writer.await??;
        let (freshness, home) = facts?;
        if cancelled.load(Ordering::Acquire) {
            return Err(SourceError::Cancelled.into());
        }

        tokio::task::spawn_blocking(move || {
            candidate.finish(
                CandidateFinish {
                    freshness,
                    home,
                    accepted_at: unix_seconds(),
                },
                current.as_ref(),
            )
        })
        .await?
        .map_err(Into::into)
    }

    async fn read_source_facts(
        self: Arc<Self>,
        batches: Sender<CandidateBatch>,
        progress: Arc<dyn Fn(SourceReadProgress) + Send + Sync>,
        cancelled: Arc<AtomicBool>,
    ) -> Result<(Option<ProviderFreshness>, HomeFacts), SourceCandidatePreparationError> {
        let (freshness, home) = match &self.implementation {
            Implementation::Local(_) => {
                let source = Arc::clone(&self);
                let is_cancelled = move || cancelled.load(Ordering::Acquire);
                let completed = tokio::task::spawn_blocking(move || {
                    let Implementation::Local(local) = &source.implementation else {
                        unreachable!("source kind changed while reading Local facts");
                    };
                    let emitter = BatchEmitter::new(batches);
                    local.read_facts(&emitter, &*progress, &is_cancelled)
                })
                .await??;
                completed
            }
            Implementation::Jellyfin(source) => {
                let emitter = BatchEmitter::new(batches);
                let is_cancelled = || cancelled.load(Ordering::Acquire);
                source
                    .read_facts(&emitter, &*progress, &is_cancelled)
                    .await?
            }
            Implementation::OpenSubsonic(source) => {
                let emitter = BatchEmitter::new(batches);
                let is_cancelled = || cancelled.load(Ordering::Acquire);
                source
                    .read_facts(&emitter, &*progress, &is_cancelled)
                    .await?
            }
        };
        Ok((freshness, home))
    }
}

fn unix_seconds() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs().min(i64::MAX as u64) as i64)
        .unwrap_or_default()
}

pub(crate) fn configured_source_name(configured: Option<String>, provider: String) -> String {
    if let Some(name) = configured.filter(|name| !name.trim().is_empty()) {
        name.trim().to_string()
    } else {
        provider
    }
}

pub(crate) fn require_source_edit(current: &SourceConfiguration, kind: &str) -> SourceResult<()> {
    if current.kind != kind {
        return Err(SourceError::InvalidRequest(
            "the source edit belongs to another provider",
        ));
    }
    Ok(())
}

pub(crate) fn comparable_address(value: &str) -> &str {
    value.trim().trim_end_matches('/')
}

pub(crate) fn edited_source_name(requested: &str, current: &str) -> String {
    let requested = requested.trim();
    if requested.is_empty() {
        current.to_string()
    } else {
        requested.to_string()
    }
}

async fn edit_remote_playlist(
    source: &impl RemotePlaylistSource,
    edit: PlaylistEdit,
) -> SourceResult<PlaylistAcceptance> {
    let playlist_id = match edit {
        PlaylistEdit::Create { name, track_ids } => {
            source.create_playlist(&name, &track_ids).await?
        }
        PlaylistEdit::Rename { playlist_id, name } => {
            source.rename_playlist(&playlist_id, &name).await?;
            playlist_id
        }
        PlaylistEdit::Delete { playlist_id } => {
            source.delete_playlist(&playlist_id).await?;
            return Ok(PlaylistAcceptance::SourceDeleted(playlist_id));
        }
        PlaylistEdit::AddTracks {
            playlist_id,
            track_ids,
        } => {
            source.add_playlist_tracks(&playlist_id, &track_ids).await?;
            playlist_id
        }
        PlaylistEdit::RemoveEntries {
            playlist_id,
            occurrence_ids,
        } => {
            source
                .remove_playlist_entries(&playlist_id, &occurrence_ids)
                .await?;
            playlist_id
        }
        PlaylistEdit::MoveEntry {
            playlist_id,
            occurrence_id,
            new_index,
        } => {
            source
                .move_playlist_entry(&playlist_id, &occurrence_id, new_index)
                .await?;
            playlist_id
        }
    };
    source
        .read_playlist_snapshot(&playlist_id)
        .await
        .map(PlaylistAcceptance::SourceSnapshot)
}

#[cfg(test)]
mod input_identity_tests {
    use super::*;
    use crate::SourceCacheMatch;
    use library::MusicFolder;

    fn music_folder(name: &str) -> MusicFolder {
        MusicFolder {
            id: MusicFolderId::new(name),
            name: name.to_string(),
            image_ref: None,
        }
    }

    #[test]
    fn sync_batch_emitter_suppresses_empty_batches() {
        let (batches, receiver) = async_channel::unbounded();
        let emitter = BatchEmitter::new(batches);

        emitter
            .emit(CandidateBatch::MusicFolders(Vec::new()))
            .expect("suppress empty batch");
        emitter
            .emit(CandidateBatch::MusicFolders(vec![music_folder("first")]))
            .expect("send populated batch");
        drop(emitter);

        let emitted = std::iter::from_fn(|| receiver.try_recv().ok()).collect::<Vec<_>>();
        assert_eq!(emitted.len(), 1);
        assert!(matches!(
            &emitted[0],
            CandidateBatch::MusicFolders(folders) if folders == &[music_folder("first")]
        ));
    }

    #[tokio::test]
    async fn closed_batch_channel_cancels_sync_and_async_senders() {
        let (sync_batches, sync_receiver) = async_channel::unbounded();
        let sync_emitter = BatchEmitter::new(sync_batches);
        drop(sync_receiver);
        assert!(matches!(
            sync_emitter.emit(CandidateBatch::MusicFolders(vec![music_folder("sync")])),
            Err(SourceError::Cancelled)
        ));

        let (async_batches, async_receiver) = async_channel::unbounded();
        let async_emitter = BatchEmitter::new(async_batches);
        drop(async_receiver);
        assert!(matches!(
            async_emitter
                .emit_async(CandidateBatch::MusicFolders(vec![music_folder("async")]))
                .await,
            Err(SourceError::Cancelled)
        ));
    }

    #[test]
    fn jellyfin_change_merge_keeps_the_latest_action_and_full_dominates() {
        let mut changed = ObservedSourceChange::jellyfin(
            BTreeSet::from(["track-one".to_string(), "track-two".to_string()]),
            BTreeSet::new(),
        );
        changed
            .merge(ObservedSourceChange::jellyfin(
                BTreeSet::from(["album-one".to_string()]),
                BTreeSet::from(["track-one".to_string()]),
            ))
            .expect("merge Jellyfin change");
        changed
            .merge(ObservedSourceChange::jellyfin(
                BTreeSet::from(["track-one".to_string()]),
                BTreeSet::from(["track-two".to_string()]),
            ))
            .expect("merge Jellyfin change");
        assert_eq!(
            changed,
            ObservedSourceChange::Jellyfin {
                upserts: BTreeSet::from(["album-one".to_string(), "track-one".to_string(),]),
                removals: BTreeSet::from(["track-two".to_string()]),
            }
        );

        changed
            .merge(ObservedSourceChange::full())
            .expect("full change dominates");
        changed
            .merge(ObservedSourceChange::jellyfin(
                BTreeSet::from(["track-three".to_string()]),
                BTreeSet::new(),
            ))
            .expect("full change remains dominant");
        assert_eq!(changed, ObservedSourceChange::full());
    }

    #[test]
    fn local_file_changes_coalesce_without_losing_a_rescan() {
        let mut changed =
            ObservedSourceChange::LocalPaths(BTreeSet::from([PathBuf::from("/music/one.flac")]));
        changed
            .merge(ObservedSourceChange::LocalPaths(BTreeSet::from([
                PathBuf::from("/music/one.flac"),
                PathBuf::from("/music/two.flac"),
            ])))
            .expect("merge Local paths");
        assert_eq!(
            changed,
            ObservedSourceChange::LocalPaths(BTreeSet::from([
                PathBuf::from("/music/one.flac"),
                PathBuf::from("/music/two.flac"),
            ]))
        );

        changed
            .merge(ObservedSourceChange::LocalRescan)
            .expect("rescan dominates Local paths");
        assert_eq!(changed, ObservedSourceChange::LocalRescan);
    }

    fn jellyfin(
        base_url: &str,
        user_id: &str,
        trust_invalid_cert: bool,
        instant_mix: bool,
        name: &str,
    ) -> SourceConfiguration {
        crate::config::encode_provider_payload(
            SourceId::new("configured:jellyfin"),
            crate::jellyfin::JELLYFIN_SOURCE_ID,
            name,
            crate::jellyfin::JellyfinSourceConfig {
                base_url: base_url.to_string(),
                server_id: None,
                user_id: user_id.to_string(),
                username: "listener".to_string(),
                trust_invalid_cert,
                use_instant_mix: instant_mix,
            }
            .into_payload(),
        )
    }

    fn subsonic(base_url: &str, username: &str, trust_invalid_cert: bool) -> SourceConfiguration {
        navidrome(base_url, username, trust_invalid_cert, 0)
    }

    fn navidrome(
        base_url: &str,
        username: &str,
        trust_invalid_cert: bool,
        library_version: u32,
    ) -> SourceConfiguration {
        crate::config::encode_provider_payload(
            SourceId::new("configured:subsonic"),
            "navidrome",
            "Server",
            crate::subsonic::SubsonicSourceConfig {
                base_url: base_url.to_string(),
                username: username.to_string(),
                trust_invalid_cert,
                navidrome_library_version: library_version,
                authentication: crate::subsonic::SubsonicAuthentication::Password,
            }
            .into_payload(),
        )
    }

    fn input_digest(configuration: &SourceConfiguration) -> [u8; 32] {
        configuration
            .input_identity()
            .expect("valid source input identity")
            .digest
    }

    #[test]
    fn remote_account_qualifies_cached_source_facts() {
        let jellyfin_a = jellyfin("https://one.example", "account", false, false, "One");
        let jellyfin_b = jellyfin("https://two.example", "account", false, false, "One");
        let jellyfin_other_user =
            jellyfin("https://one.example", "other-account", false, false, "One");
        assert_eq!(input_digest(&jellyfin_a), input_digest(&jellyfin_b));
        assert_ne!(
            input_digest(&jellyfin_a),
            input_digest(&jellyfin_other_user)
        );
        assert_eq!(
            jellyfin_other_user
                .cache_match(
                    &jellyfin_a
                        .input_identity()
                        .expect("Jellyfin input identity")
                )
                .expect("classify another Jellyfin account's cache"),
            SourceCacheMatch::Incompatible
        );

        let subsonic_a = subsonic("https://one.example", "listener", false);
        let subsonic_b = subsonic("https://two.example", "listener", false);
        let subsonic_other_user = subsonic("https://one.example", "other", false);
        assert_eq!(input_digest(&subsonic_a), input_digest(&subsonic_b));
        assert_ne!(
            input_digest(&subsonic_a),
            input_digest(&subsonic_other_user)
        );
    }

    #[test]
    fn presentation_and_transport_policy_do_not_invalidate_cached_facts() {
        let baseline = jellyfin("https://music.example", "account", false, false, "Music");
        let presentation = jellyfin("https://music.example/", "account", true, true, "Renamed");
        assert_eq!(input_digest(&baseline), input_digest(&presentation));

        let subsonic_baseline = subsonic("https://music.example", "listener", false);
        let subsonic_trust = subsonic("https://music.example/", "listener", true);
        assert_eq!(
            input_digest(&subsonic_baseline),
            input_digest(&subsonic_trust)
        );
    }

    #[test]
    fn navidrome_library_reader_qualifies_cached_source_facts() {
        let generic = navidrome("https://music.example", "listener", false, 0);
        let private_library = navidrome("https://music.example", "listener", false, 1);
        let other_user = navidrome("https://music.example", "other", false, 0);

        assert_ne!(input_digest(&generic), input_digest(&private_library));
        assert_eq!(
            private_library
                .cache_match(&generic.input_identity().expect("generic input identity"))
                .expect("classify generic Navidrome cache"),
            SourceCacheMatch::ReaderUpgrade
        );
        assert_eq!(
            private_library
                .cache_match(
                    &private_library
                        .input_identity()
                        .expect("native input identity")
                )
                .expect("classify native Navidrome cache"),
            SourceCacheMatch::Exact
        );
        assert_eq!(
            private_library
                .cache_match(&other_user.input_identity().expect("other input identity"))
                .expect("classify another account's cache"),
            SourceCacheMatch::Incompatible
        );
    }

    #[test]
    fn local_roots_qualify_cached_source_facts() {
        let directory = tempfile::tempdir().expect("temporary Local roots");
        let first = SourceConfiguration::local(
            SourceId::new(crate::local::LOCAL_LIBRARY_SOURCE_ID),
            "Local",
            vec![directory.path().join("one")],
        )
        .expect("first Local configuration");
        let second = SourceConfiguration::local(
            SourceId::new(crate::local::LOCAL_LIBRARY_SOURCE_ID),
            "Local",
            vec![directory.path().join("two")],
        )
        .expect("second Local configuration");
        assert_ne!(input_digest(&first), input_digest(&second));
        assert_eq!(
            second
                .cache_match(&first.input_identity().expect("first Local input identity"))
                .expect("classify another Local root cache"),
            SourceCacheMatch::Incompatible
        );
    }

    #[tokio::test]
    async fn local_home_refresh_stays_at_the_library_boundary() {
        let directory = tempfile::tempdir().expect("temporary Local root parent");
        let source = Source::open(
            crate::config::encode_provider_payload(
                SourceId::new(crate::local::LOCAL_LIBRARY_SOURCE_ID),
                crate::local::LOCAL_SOURCE_ID,
                "Local",
                crate::local::LocalSourceConfig {
                    roots: vec![directory.path().join("unavailable")],
                }
                .into_payload(),
            ),
            None,
            None,
        )
        .expect("open Local fixture");

        for kind in [
            HomeSectionKind::Explore,
            HomeSectionKind::MostPlayed,
            HomeSectionKind::NewlyAdded,
            HomeSectionKind::RecentlyPlayed,
            HomeSectionKind::RecentlyReleased,
        ] {
            assert!(matches!(
                source
                    .home_section(kind)
                    .await
                    .expect("Local Home boundary"),
                NativeSourceResult::Unavailable
            ));
        }
    }
}
