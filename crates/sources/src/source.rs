//! Connects configured providers to Library's concrete Scan and point operations.
//! Provider acquisition remains here; accepted catalog identity and queries remain in Library.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use async_channel::{Receiver, Sender};
use library::{Database, Freshness, Scan, ScanOutcome};
use playback::{ResolvedStream, SourceReportFact, StreamRequest};
use serde::{Deserialize, Serialize};

use crate::policy::raw_item_id;
use crate::{
    ImageBytes, SourceConfiguration, SourceError, SourceId, SourceResult, SourceSettingsInput,
    SourceSetupInput,
};

const PROVIDER_PLAYLIST_PAGE: usize = 256;

pub(crate) const LIVE_CHANGE_LIMIT: usize = 128;

pub struct SelectedFeed {
    source: Arc<Source>,
    database: Arc<Database>,
    source_key: library::SourceKey,
    pending: Mutex<Option<SelectedFeedChange>>,
    wake: Sender<()>,
    cancelled: Arc<AtomicBool>,
    receiver: Receiver<()>,
}

enum SelectedFeedChange {
    Local(LocalLiveChange),
    Jellyfin(JellyfinLiveChange),
}

impl SelectedFeed {
    fn submit(&self, incoming: SelectedFeedChange) -> bool {
        if self.cancelled.load(Ordering::Acquire) {
            return false;
        }
        let mut pending = self
            .pending
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        *pending = Some(match pending.take() {
            Some(current) => current.merge(incoming),
            None => incoming,
        });
        drop(pending);
        let _ = self.wake.try_send(());
        true
    }
}

impl SelectedFeedChange {
    fn merge(self, incoming: Self) -> Self {
        match (self, incoming) {
            (Self::Local(current), Self::Local(incoming)) => Self::Local(current.merge(incoming)),
            (Self::Jellyfin(current), Self::Jellyfin(incoming)) => {
                Self::Jellyfin(current.merge(incoming))
            }
            (_, incoming) => incoming,
        }
    }
}

impl SelectedFeed {
    pub fn cancel(&self) {
        self.cancelled.store(true, Ordering::Release);
        let _ = self.wake.try_send(());
    }

    pub fn cancellation(&self) -> Arc<AtomicBool> {
        Arc::clone(&self.cancelled)
    }

    pub async fn wait_for_change(&self) -> bool {
        loop {
            if self.receiver.recv().await.is_err() {
                return false;
            }
            if self.cancelled.load(Ordering::Acquire) {
                return false;
            }
            if self
                .pending
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .is_some()
            {
                return true;
            }
        }
    }

    pub async fn apply_pending(&self) -> SourceResult<Option<ScanOutcome>> {
        if self.cancelled.load(Ordering::Acquire) {
            return Err(SourceError::Cancelled);
        }
        let change = self
            .pending
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .take()
            .ok_or(SourceError::InvalidRequest(
                "Selected feed has no pending change",
            ))?;
        match (&self.source.implementation, change) {
            (Implementation::Local(_), SelectedFeedChange::Local(LocalLiveChange::Rescan))
            | (
                Implementation::Jellyfin(_),
                SelectedFeedChange::Jellyfin(JellyfinLiveChange::BoundaryLost),
            ) => Ok(None),
            (
                Implementation::Local(local),
                SelectedFeedChange::Local(LocalLiveChange::Paths { paths, rename }),
            ) => local
                .publish_paths(
                    &self.database,
                    self.source_key,
                    self.source.source_id.as_str(),
                    &paths,
                    rename.as_ref(),
                )
                .await
                .map(Some),
            (
                Implementation::Jellyfin(source),
                SelectedFeedChange::Jellyfin(JellyfinLiveChange::Items { upserts, removals }),
            ) => source
                .apply_live_items(
                    &self.database,
                    self.source.source_id.as_str(),
                    upserts,
                    removals,
                )
                .await
                .map(Some),
            _ => Err(SourceError::InvalidRequest(
                "Selected feed change belongs to another source",
            )),
        }
    }
}

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
pub struct SourceInputIdentity {
    pub source_id: SourceId,
    pub digest: [u8; 32],
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct NativeImageRef {
    pub item_id: String,
    pub tag: Option<String>,
}

impl NativeImageRef {
    pub fn new(item_id: impl Into<String>, tag: impl Into<Option<String>>) -> Self {
        Self {
            item_id: item_id.into(),
            tag: tag.into(),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum LocalImageRef {
    File {
        path: String,
        revision: String,
    },
    Embedded {
        path: String,
        picture_index: u32,
        revision: String,
    },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SourceImageRequest {
    Native {
        image_ref: NativeImageRef,
        size: u32,
    },
    Local(LocalImageRef),
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ExternalAlbumImageRef {
    pub artist: String,
    pub album: String,
    pub musicbrainz_release_group_id: Option<String>,
    pub musicbrainz_release_id: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ArtworkPreparationProgress {
    pub completed: usize,
    pub next_album: Option<library::AlbumKey>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LiveSearchTrack {
    pub object_id: String,
    pub title: String,
    pub artist: String,
    pub album: String,
    pub artwork_binding: Option<Vec<u8>>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LiveSearchAlbum {
    pub object_id: String,
    pub title: String,
    pub artist: String,
    pub artwork_binding: Option<Vec<u8>>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LiveSearchArtist {
    pub object_id: String,
    pub name: String,
    pub artwork_binding: Option<Vec<u8>>,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct LiveSearchResults {
    pub tracks: Vec<LiveSearchTrack>,
    pub albums: Vec<LiveSearchAlbum>,
    pub artists: Vec<LiveSearchArtist>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LiveFolder {
    pub object_id: String,
    pub name: String,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct LiveFolderPage {
    pub folders: Vec<LiveFolder>,
    pub tracks: Vec<String>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SourceEntityKind {
    Track,
    Album,
    Artist,
}
pub enum SourceRadioSeed {
    Track(String),
    Album(String),
    Artist(String),
    Playlist(String),
    Genre(String),
}

pub enum SourceCollection {
    Album(String),
    Artist(String),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SourceHomeSection {
    MostPlayed,
    RecentlyPlayed,
    RecentlyReleased,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum JellyfinLiveChange {
    Items {
        upserts: Vec<String>,
        removals: Vec<String>,
    },
    BoundaryLost,
}

impl JellyfinLiveChange {
    fn merge(self, incoming: Self) -> Self {
        match (self, incoming) {
            (Self::BoundaryLost, _) | (_, Self::BoundaryLost) => Self::BoundaryLost,
            (
                Self::Items {
                    upserts: mut current_upserts,
                    removals: mut current_removals,
                },
                Self::Items { upserts, removals },
            ) => {
                current_upserts.extend(upserts);
                current_upserts.sort();
                current_upserts.dedup();
                current_removals.extend(removals);
                current_removals.sort();
                current_removals.dedup();
                if current_upserts.len().saturating_add(current_removals.len()) > LIVE_CHANGE_LIMIT
                    || current_upserts
                        .iter()
                        .any(|id| current_removals.binary_search(id).is_ok())
                {
                    Self::BoundaryLost
                } else {
                    Self::Items {
                        upserts: current_upserts,
                        removals: current_removals,
                    }
                }
            }
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum LocalLiveChange {
    Paths {
        paths: Vec<PathBuf>,
        rename: Option<(PathBuf, PathBuf)>,
    },
    Rescan,
}

impl LocalLiveChange {
    pub(crate) fn merge(self, incoming: Self) -> Self {
        match (self, incoming) {
            (Self::Rescan, _) | (_, Self::Rescan) => Self::Rescan,
            (
                Self::Paths {
                    paths: mut current,
                    rename: current_rename,
                },
                Self::Paths {
                    paths: incoming,
                    rename: incoming_rename,
                },
            ) => {
                let rename = match (current_rename, incoming_rename) {
                    (Some(current), Some(incoming)) if current != incoming => {
                        return Self::Rescan;
                    }
                    (Some(rename), _) | (_, Some(rename)) => Some(rename),
                    (None, None) => None,
                };
                current.extend(incoming);
                current.sort();
                current.dedup();
                if current.len() > LIVE_CHANGE_LIMIT {
                    Self::Rescan
                } else {
                    Self::Paths {
                        paths: current,
                        rename,
                    }
                }
            }
        }
    }
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

pub struct ConnectedSource {
    configuration: SourceConfiguration,
    source: Source,
    credential: Option<String>,
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

pub enum SourceEditResult {
    Unchanged,
    ConfigurationOnly(SourceConfiguration),
    Connected(Box<ConnectedSource>),
}

enum Implementation {
    Local(crate::local::LocalSource),
    Jellyfin(crate::jellyfin::JellyfinSource),
    OpenSubsonic(crate::subsonic::SubsonicSource),
}

pub struct Source {
    source_id: SourceId,
    implementation: Implementation,
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

    pub async fn refresh_if_needed(
        &self,
        database: &Database,
        display_name: &str,
        progress: &(dyn Fn(SourceReadProgress) + Send + Sync),
        cancelled: Arc<AtomicBool>,
    ) -> SourceResult<Option<ScanOutcome>> {
        let Some(freshness) = self.freshness().await? else {
            return Ok(None);
        };
        let cancellation = library::ReadCancellation::new();
        if cancelled.load(Ordering::Relaxed) {
            cancellation.cancel();
        }
        if let Some(publication) =
            Scan::accept_freshness(database, self.source_id.as_str(), &freshness, &cancellation)
                .await?
        {
            return Ok(Some(ScanOutcome::Identical(publication)));
        }
        self.refresh(
            database,
            display_name,
            progress,
            cancelled,
            Some(freshness),
            false,
        )
        .await
        .map(Some)
    }

    pub async fn manual_refresh(
        &self,
        database: &Database,
        display_name: &str,
        progress: &(dyn Fn(SourceReadProgress) + Send + Sync),
        cancelled: Arc<AtomicBool>,
    ) -> SourceResult<ScanOutcome> {
        let freshness = self.freshness().await?;
        let reuse_local = matches!(&self.implementation, Implementation::Local(_));
        self.refresh(
            database,
            display_name,
            progress,
            cancelled,
            freshness,
            reuse_local,
        )
        .await
    }

    async fn refresh(
        &self,
        database: &Database,
        display_name: &str,
        progress: &(dyn Fn(SourceReadProgress) + Send + Sync),
        cancelled: Arc<AtomicBool>,
        freshness: Option<Freshness>,
        reuse_local: bool,
    ) -> SourceResult<ScanOutcome> {
        let mut scan = Scan::begin(
            database,
            self.source_id.as_str(),
            display_name,
            &display_name.to_lowercase(),
            freshness,
        )
        .await?;
        let cancelled_fn = || cancelled.load(Ordering::Relaxed);
        match &self.implementation {
            Implementation::Local(source) => {
                source
                    .stage_catalog(database, &mut scan, progress, &cancelled_fn, reuse_local)
                    .await?
            }
            Implementation::Jellyfin(source) => {
                source
                    .stage_catalog(&mut scan, progress, &cancelled_fn)
                    .await?
            }
            Implementation::OpenSubsonic(source) => {
                source
                    .stage_catalog(&mut scan, progress, &cancelled_fn)
                    .await?
            }
        }
        let outcome = scan.finish().await?;
        Ok(outcome)
    }

    pub async fn catch_up_local(
        &self,
        database: &Database,
        source: library::SourceKey,
        progress: &(dyn Fn(SourceReadProgress) + Send + Sync),
        cancelled: Arc<AtomicBool>,
    ) -> SourceResult<ScanOutcome> {
        let Implementation::Local(local) = &self.implementation else {
            return Err(SourceError::InvalidRequest(
                "Local catch-up belongs to another source",
            ));
        };
        local
            .catch_up(database, source, self.source_id.as_str(), progress, &|| {
                cancelled.load(Ordering::Relaxed)
            })
            .await
    }

    async fn freshness(&self) -> SourceResult<Option<Freshness>> {
        match &self.implementation {
            Implementation::Local(_) | Implementation::Jellyfin(_) => Ok(None),
            Implementation::OpenSubsonic(source) => source.freshness().await,
        }
    }

    pub async fn stream(&self, request: StreamRequest) -> SourceResult<ResolvedStream> {
        if let Some(uri) = request.media_uri.as_deref() {
            let mut stream = ResolvedStream::new(uri);
            if let (Some(start), Some(end)) = (request.cue_start_millis, request.cue_end_millis) {
                stream = stream.with_window(start, end);
            }
            return Ok(stream);
        }
        match &self.implementation {
            Implementation::Local(_) => Err(SourceError::NotFound),
            Implementation::Jellyfin(source) => source.resolve_stream(&request).await,
            Implementation::OpenSubsonic(source) => source.resolve_stream(&request).await,
        }
    }

    pub fn resolve_download(&self, request: &StreamRequest) -> SourceResult<ResolvedDownload> {
        if let Some(uri) = request.media_uri.as_deref() {
            let mut stream = ResolvedStream::new(uri);
            if let (Some(start), Some(end)) = (request.cue_start_millis, request.cue_end_millis) {
                stream = stream.with_window(start, end);
            }
            return Ok(ResolvedDownload::new(stream, None));
        }
        match &self.implementation {
            Implementation::Local(_) => Err(SourceError::NotFound),
            Implementation::Jellyfin(source) => source.resolve_download(request),
            Implementation::OpenSubsonic(source) => source.resolve_download(request),
        }
    }

    pub async fn image(&self, request: SourceImageRequest) -> SourceResult<ImageBytes> {
        match &self.implementation {
            Implementation::Local(source) => source.image(request),
            Implementation::Jellyfin(source) => match request {
                SourceImageRequest::Native { image_ref, size } => {
                    source.image_bytes(&image_ref, size).await
                }
                SourceImageRequest::Local(_) => Err(SourceError::NotFound),
            },
            Implementation::OpenSubsonic(source) => match request {
                SourceImageRequest::Native { image_ref, size } => {
                    source.image_bytes(&image_ref, size).await
                }
                SourceImageRequest::Local(_) => Err(SourceError::NotFound),
            },
        }
    }

    pub async fn live_search(&self, query: &str, limit: usize) -> SourceResult<LiveSearchResults> {
        match &self.implementation {
            Implementation::Local(_) => Err(SourceError::InvalidRequest(
                "Local Search is Database-owned",
            )),
            Implementation::Jellyfin(source) => source.live_search(query, limit).await,
            Implementation::OpenSubsonic(source) => source.live_search(query, limit).await,
        }
    }

    pub async fn set_favorite(
        &self,
        kind: SourceEntityKind,
        object_id: &str,
        favorite: bool,
    ) -> SourceResult<()> {
        match &self.implementation {
            Implementation::Local(_) => Ok(()),
            Implementation::Jellyfin(source) => source.set_favorite(object_id, favorite).await,
            Implementation::OpenSubsonic(source) => {
                source.set_favorite(kind, object_id, favorite).await
            }
        }
    }

    pub async fn set_rating(&self, object_id: &str, rating: Option<u8>) -> SourceResult<()> {
        if rating.is_some_and(|rating| rating > 10) {
            return Err(SourceError::InvalidRequest("rating outside 0..=10"));
        }
        match &self.implementation {
            Implementation::Local(_) => Ok(()),
            Implementation::Jellyfin(source) => source.set_rating(object_id, rating).await,
            Implementation::OpenSubsonic(source) => source.set_rating(object_id, rating).await,
        }
    }

    pub async fn write_local_track_rating(
        &self,
        database: &Database,
        source: library::SourceKey,
        track: library::TrackKey,
        rating: Option<u8>,
    ) -> SourceResult<()> {
        let Implementation::Local(local) = &self.implementation else {
            return Ok(());
        };
        let row = database
            .track_rows(source, &[track], &library::ReadCancellation::new())
            .await?
            .pop()
            .ok_or(SourceError::NotFound)?;
        let Some((path, format)) = self.metadata_track_path(database, source, &row).await? else {
            return Err(SourceError::NotFound);
        };
        crate::local::metadata::write_rating(&path, format.as_deref(), rating)
            .map_err(|error| SourceError::Other(error.to_string()))?;
        local
            .publish_paths(database, source, self.source_id.as_str(), &[path], None)
            .await?;
        Ok(())
    }

    async fn mapped_metadata_ready(&self) -> bool {
        let Implementation::OpenSubsonic(source) = &self.implementation else {
            return false;
        };
        if !source.metadata_editing_available() {
            source.refresh_metadata_editing().await;
        }
        source.metadata_editing_available()
    }

    async fn refresh_remote_metadata_index(&self) -> SourceResult<()> {
        match &self.implementation {
            Implementation::Local(_) | Implementation::Jellyfin(_) => Ok(()),
            Implementation::OpenSubsonic(source) => {
                source.require_metadata_scan_idle().await?;
                source.start_metadata_scan_and_wait().await
            }
        }
    }

    pub async fn lyrics_writable(
        &self,
        database: &Database,
        source: library::SourceKey,
        track_key: library::TrackKey,
    ) -> bool {
        let Ok(Some(track)) = database
            .track_rows(source, &[track_key], &library::ReadCancellation::new())
            .await
            .map(|mut rows| rows.pop())
        else {
            return false;
        };
        if track.cue_path.is_some() {
            return false;
        }
        match &self.implementation {
            Implementation::Jellyfin(_) => true,
            Implementation::Local(_) | Implementation::OpenSubsonic(_) => {
                let Ok((path, _)) = self.metadata_file_target(database, source, &track).await
                else {
                    return false;
                };
                (!matches!(self.implementation, Implementation::OpenSubsonic(_))
                    || self.mapped_metadata_ready().await)
                    && crate::local::embedded_lyrics_writable(&path)
            }
        }
    }

    pub async fn write_lyrics(
        &self,
        database: &Database,
        source: library::SourceKey,
        track_key: library::TrackKey,
        lyrics: &str,
    ) -> Result<(), crate::SourceMetadataError> {
        let track = database
            .track_rows(source, &[track_key], &library::ReadCancellation::new())
            .await
            .map_err(metadata_database_error)?
            .pop()
            .ok_or(crate::SourceMetadataError::Unavailable)?;
        if track.cue_path.is_some() {
            return Err(crate::SourceMetadataError::Unavailable);
        }
        match &self.implementation {
            Implementation::Jellyfin(jellyfin) => jellyfin
                .write_lyrics(&track.object_id, lyrics)
                .await
                .map_err(|error| crate::SourceMetadataError::Write(error.to_string())),
            Implementation::Local(local) => {
                let path = self
                    .write_file_lyrics(database, source, &track, lyrics)
                    .await?;
                local
                    .publish_paths(database, source, self.source_id.as_str(), &[path], None)
                    .await
                    .map(|_| ())
                    .map_err(|error| {
                        crate::SourceMetadataError::SavedRefreshFailed(error.to_string())
                    })
            }
            Implementation::OpenSubsonic(_) => {
                if !self.mapped_metadata_ready().await {
                    return Err(crate::SourceMetadataError::Unavailable);
                }
                self.write_file_lyrics(database, source, &track, lyrics)
                    .await
                    .map(|_| ())
            }
        }
    }

    pub async fn refresh_written_lyrics(&self) -> Result<(), crate::SourceMetadataError> {
        self.refresh_remote_metadata_index()
            .await
            .map_err(|error| crate::SourceMetadataError::SavedRefreshFailed(error.to_string()))
    }

    async fn write_file_lyrics(
        &self,
        database: &Database,
        source: library::SourceKey,
        track: &library::TrackRow,
        lyrics: &str,
    ) -> Result<PathBuf, crate::SourceMetadataError> {
        let (path, _) = self.metadata_file_target(database, source, track).await?;
        if !crate::local::embedded_lyrics_writable(&path) {
            return Err(crate::SourceMetadataError::Unavailable);
        }
        crate::local::write_embedded_lyrics(&path, lyrics)?;
        Ok(path)
    }

    pub async fn read_track_metadata(
        &self,
        database: &Database,
        source: library::SourceKey,
        track_key: library::TrackKey,
    ) -> Result<crate::TrackMetadata, crate::SourceMetadataError> {
        let cancellation = library::ReadCancellation::new();
        let track = database
            .track_rows(source, &[track_key], &cancellation)
            .await
            .map_err(|error| crate::SourceMetadataError::Write(error.to_string()))?
            .pop()
            .ok_or(crate::SourceMetadataError::Unavailable)?;
        if let Implementation::Jellyfin(jellyfin) = &self.implementation {
            return jellyfin.read_track_metadata(track).await;
        }
        let (path, format) = self.metadata_file_target(database, source, &track).await?;
        if matches!(self.implementation, Implementation::OpenSubsonic(_))
            && !self.mapped_metadata_ready().await
        {
            return Err(crate::SourceMetadataError::Unavailable);
        }
        let mut metadata = crate::local::read_track_metadata(track_key, &path, format.as_deref())?;
        if metadata.values.musicbrainz_recording_id.is_none()
            && track.musicbrainz_recording_id.is_some()
        {
            metadata.values.musicbrainz_recording_id = track.musicbrainz_recording_id;
            metadata.rufin_filled.musicbrainz_recording_id = true;
        }
        if metadata.values.musicbrainz_release_track_id.is_none()
            && track.musicbrainz_release_track_id.is_some()
        {
            metadata.values.musicbrainz_release_track_id = track.musicbrainz_release_track_id;
            metadata.rufin_filled.musicbrainz_release_track_id = true;
        }
        Ok(metadata)
    }

    pub async fn read_album_metadata(
        &self,
        database: &Database,
        source: library::SourceKey,
        album_key: library::AlbumKey,
    ) -> Result<crate::AlbumMetadata, crate::SourceMetadataError> {
        let cancellation = library::ReadCancellation::new();
        let album = database
            .album_rows(source, &[album_key], None, &cancellation)
            .await
            .map_err(metadata_database_error)?
            .pop()
            .ok_or(crate::SourceMetadataError::Unavailable)?;
        match &self.implementation {
            Implementation::Jellyfin(jellyfin) => jellyfin.read_album_metadata(album).await,
            Implementation::Local(_) => {
                self.read_local_album_metadata(database, source, album)
                    .await
            }
            Implementation::OpenSubsonic(_) => {
                let metadata = self
                    .read_local_album_metadata(database, source, album)
                    .await?;
                if !self.mapped_metadata_ready().await {
                    return Err(crate::SourceMetadataError::Unavailable);
                }
                Ok(metadata)
            }
        }
    }

    pub async fn read_artist_metadata(
        &self,
        database: &Database,
        source: library::SourceKey,
        artist_key: library::ArtistKey,
    ) -> Result<crate::ArtistMetadata, crate::SourceMetadataError> {
        let cancellation = library::ReadCancellation::new();
        let artist = database
            .artist_rows(source, &[artist_key], false, None, &cancellation)
            .await
            .map_err(metadata_database_error)?
            .pop()
            .ok_or(crate::SourceMetadataError::Unavailable)?;
        match &self.implementation {
            Implementation::Jellyfin(jellyfin) => jellyfin.read_artist_metadata(artist).await,
            Implementation::Local(_) => {
                self.read_local_artist_metadata(database, source, artist)
                    .await
            }
            Implementation::OpenSubsonic(_) => {
                let metadata = self
                    .read_local_artist_metadata(database, source, artist)
                    .await?;
                if !self.mapped_metadata_ready().await {
                    return Err(crate::SourceMetadataError::Unavailable);
                }
                Ok(metadata)
            }
        }
    }

    pub async fn identify_album_metadata(
        &self,
        database: &Database,
        source: library::SourceKey,
        album_key: library::AlbumKey,
        values: &crate::AlbumMetadataValues,
    ) -> Result<Option<(crate::AlbumMetadataValues, String)>, String> {
        let Implementation::Jellyfin(jellyfin) = &self.implementation else {
            return Ok(None);
        };
        let cancellation = library::ReadCancellation::new();
        let album = database
            .album_rows(source, &[album_key], None, &cancellation)
            .await
            .map_err(|error| error.to_string())?
            .pop()
            .ok_or_else(|| "Album is no longer current".to_string())?;
        jellyfin
            .identify_album_metadata(&album.object_id, values)
            .await
    }

    pub async fn identify_artist_metadata(
        &self,
        database: &Database,
        source: library::SourceKey,
        artist_key: library::ArtistKey,
        values: &crate::ArtistMetadataValues,
    ) -> Result<Option<(crate::ArtistMetadataValues, String)>, String> {
        let Implementation::Jellyfin(jellyfin) = &self.implementation else {
            return Ok(None);
        };
        let cancellation = library::ReadCancellation::new();
        let artist = database
            .artist_rows(source, &[artist_key], false, None, &cancellation)
            .await
            .map_err(|error| error.to_string())?
            .pop()
            .ok_or_else(|| "Artist is no longer current".to_string())?;
        jellyfin
            .identify_artist_metadata(&artist.object_id, values)
            .await
    }

    pub async fn write_track_metadata(
        &self,
        database: &Database,
        source: library::SourceKey,
        track_key: library::TrackKey,
        expected_revision: &str,
        application: Option<&str>,
        edit: crate::TrackMetadataEdit,
    ) -> Result<library::TrackRow, crate::SourceMetadataError> {
        let cancellation = library::ReadCancellation::new();
        let track = database
            .track_rows(source, &[track_key], &cancellation)
            .await
            .map_err(metadata_database_error)?
            .pop()
            .ok_or(crate::SourceMetadataError::Unavailable)?;
        match &self.implementation {
            Implementation::Jellyfin(jellyfin) => {
                let raw = jellyfin
                    .write_metadata_value(
                        &track.object_id,
                        "track",
                        expected_revision,
                        application,
                        |item| super::jellyfin::metadata::apply_track_edit(item, &edit),
                    )
                    .await?;
                self.publish_jellyfin_metadata(database, raw).await?;
            }
            Implementation::Local(_) | Implementation::OpenSubsonic(_) => {
                self.write_file_track_metadata(database, source, &track, expected_revision, &edit)
                    .await?;
            }
        }
        database
            .track_rows(source, &[track_key], &cancellation)
            .await
            .map_err(metadata_database_error)?
            .pop()
            .ok_or(crate::SourceMetadataError::Unavailable)
    }

    pub async fn write_album_metadata(
        &self,
        database: &Database,
        source: library::SourceKey,
        album_key: library::AlbumKey,
        expected_revision: &str,
        application: Option<&str>,
        edit: crate::AlbumMetadataEdit,
    ) -> Result<library::AlbumRow, crate::SourceMetadataError> {
        let cancellation = library::ReadCancellation::new();
        let album = database
            .album_rows(source, &[album_key], None, &cancellation)
            .await
            .map_err(metadata_database_error)?
            .pop()
            .ok_or(crate::SourceMetadataError::Unavailable)?;
        match &self.implementation {
            Implementation::Jellyfin(jellyfin) => {
                let raw = jellyfin
                    .write_metadata_value(
                        &album.object_id,
                        "album",
                        expected_revision,
                        application,
                        |item| super::jellyfin::metadata::apply_album_edit(item, &edit),
                    )
                    .await?;
                self.publish_jellyfin_metadata(database, raw).await?;
            }
            Implementation::Local(_) | Implementation::OpenSubsonic(_) => {
                self.write_file_album_metadata(database, source, &album, expected_revision, &edit)
                    .await?;
            }
        }
        database
            .album_rows(source, &[album_key], None, &cancellation)
            .await
            .map_err(metadata_database_error)?
            .pop()
            .ok_or(crate::SourceMetadataError::Unavailable)
    }

    pub async fn write_artist_metadata(
        &self,
        database: &Database,
        source: library::SourceKey,
        artist_key: library::ArtistKey,
        expected_revision: &str,
        application: Option<&str>,
        edit: crate::ArtistMetadataEdit,
    ) -> Result<library::ArtistRow, crate::SourceMetadataError> {
        let cancellation = library::ReadCancellation::new();
        let artist = database
            .artist_rows(source, &[artist_key], false, None, &cancellation)
            .await
            .map_err(metadata_database_error)?
            .pop()
            .ok_or(crate::SourceMetadataError::Unavailable)?;
        match &self.implementation {
            Implementation::Jellyfin(jellyfin) => {
                let raw = jellyfin
                    .write_metadata_value(
                        &artist.object_id,
                        "artist",
                        expected_revision,
                        application,
                        |item| super::jellyfin::metadata::apply_artist_edit(item, &edit),
                    )
                    .await?;
                self.publish_jellyfin_metadata(database, raw).await?;
            }
            Implementation::Local(_) | Implementation::OpenSubsonic(_) => {
                self.write_file_artist_metadata(
                    database,
                    source,
                    &artist,
                    expected_revision,
                    &edit,
                )
                .await?;
            }
        }
        database
            .artist_rows(source, &[artist_key], false, None, &cancellation)
            .await
            .map_err(metadata_database_error)?
            .pop()
            .ok_or(crate::SourceMetadataError::Unavailable)
    }

    async fn publish_jellyfin_metadata(
        &self,
        database: &Database,
        raw: String,
    ) -> Result<(), crate::SourceMetadataError> {
        let Implementation::Jellyfin(jellyfin) = &self.implementation else {
            return Err(crate::SourceMetadataError::Unavailable);
        };
        jellyfin
            .apply_live_items(database, self.source_id.as_str(), vec![raw], Vec::new())
            .await
            .map_err(|error| crate::SourceMetadataError::SavedRefreshFailed(error.to_string()))?;
        Ok(())
    }

    pub async fn write_local_r128_tags(
        &self,
        database: &Database,
        source: library::SourceKey,
        measurements: &[(library::TrackKey, Option<f64>, Option<f64>)],
    ) -> Result<(), crate::SourceMetadataError> {
        let Implementation::Local(local) = &self.implementation else {
            return Err(crate::SourceMetadataError::Unavailable);
        };
        if measurements.is_empty() {
            return Ok(());
        }
        let keys = measurements
            .iter()
            .map(|(track, _, _)| *track)
            .collect::<Vec<_>>();
        let measurements = measurements
            .iter()
            .map(|(track, track_lufs, album_lufs)| (*track, (*track_lufs, *album_lufs)))
            .collect::<HashMap<_, _>>();
        let rows = database
            .track_rows(source, &keys, &library::ReadCancellation::new())
            .await
            .map_err(metadata_database_error)?;
        let mut written = Vec::new();
        for row in rows {
            let Some((track_lufs, album_lufs)) = measurements.get(&row.track_key) else {
                continue;
            };
            let Some((path, format)) = self
                .metadata_track_path(database, source, &row)
                .await
                .map_err(|error| crate::SourceMetadataError::Write(error.to_string()))?
            else {
                continue;
            };
            match crate::local::metadata::write_r128(
                &path,
                format.as_deref(),
                *track_lufs,
                *album_lufs,
            ) {
                Ok(()) => written.push(path),
                Err(crate::SourceMetadataError::Unavailable) => {}
                Err(error) => return Err(error),
            }
        }
        if !written.is_empty() {
            local
                .publish_metadata_paths(database, self.source_id.as_str(), &written, None, None)
                .await
                .map_err(|error| {
                    crate::SourceMetadataError::SavedRefreshFailed(error.to_string())
                })?;
        }
        Ok(())
    }

    pub async fn backfill_local_r128_tags(
        &self,
        database: &Database,
        source: library::SourceKey,
    ) -> Result<(), crate::SourceMetadataError> {
        if !matches!(&self.implementation, Implementation::Local(_)) {
            return Err(crate::SourceMetadataError::Unavailable);
        }
        let mut after = None;
        loop {
            let page = database
                .r128_tag_write_page(source, after, 64, &library::ReadCancellation::new())
                .await
                .map_err(metadata_database_error)?;
            if page.is_empty() {
                return Ok(());
            }
            after = page.last().map(|item| item.track_key);
            let measurements = page
                .into_iter()
                .map(|item| (item.track_key, Some(item.track_lufs), item.album_lufs))
                .collect::<Vec<_>>();
            self.write_local_r128_tags(database, source, &measurements)
                .await?;
        }
    }

    async fn read_local_album_metadata(
        &self,
        database: &Database,
        source: library::SourceKey,
        album: library::AlbumRow,
    ) -> Result<crate::AlbumMetadata, crate::SourceMetadataError> {
        let targets = self
            .album_metadata_targets(database, source, album.album_key)
            .await?;
        album_metadata_from_targets(album, &targets)
    }

    async fn read_local_artist_metadata(
        &self,
        database: &Database,
        source: library::SourceKey,
        artist: library::ArtistRow,
    ) -> Result<crate::ArtistMetadata, crate::SourceMetadataError> {
        let targets = self
            .artist_metadata_targets(database, source, artist.artist_key)
            .await?;
        artist_metadata_from_targets(artist, &targets)
    }

    async fn write_file_track_metadata(
        &self,
        database: &Database,
        source: library::SourceKey,
        track: &library::TrackRow,
        expected_revision: &str,
        edit: &crate::TrackMetadataEdit,
    ) -> Result<(), crate::SourceMetadataError> {
        let (path, format) = self.metadata_file_target(database, source, track).await?;
        crate::local::metadata::write_track(&path, format.as_deref(), expected_revision, edit)?;
        match &self.implementation {
            Implementation::Local(local) => {
                local
                    .publish_metadata_paths(
                        database,
                        self.source_id.as_str(),
                        std::slice::from_ref(&path),
                        None,
                        None,
                    )
                    .await
                    .map_err(|error| {
                        crate::SourceMetadataError::SavedRefreshFailed(error.to_string())
                    })?;
            }
            Implementation::OpenSubsonic(_) => {
                self.refresh_remote_metadata_index()
                    .await
                    .map_err(|error| {
                        crate::SourceMetadataError::SavedRefreshFailed(error.to_string())
                    })?;
                database
                    .update_track_metadata(
                        source,
                        track.track_key,
                        track_write_from_values(track, &edit.values),
                    )
                    .await
                    .map_err(metadata_database_error)?;
            }
            Implementation::Jellyfin(_) => unreachable!(),
        }
        Ok(())
    }

    async fn write_file_album_metadata(
        &self,
        database: &Database,
        source: library::SourceKey,
        album: &library::AlbumRow,
        expected_revision: &str,
        edit: &crate::AlbumMetadataEdit,
    ) -> Result<(), crate::SourceMetadataError> {
        let targets = self
            .album_metadata_targets(database, source, album.album_key)
            .await?;
        let paths = targets
            .iter()
            .map(|target| target.path.clone())
            .collect::<Vec<_>>();
        let file_targets = targets
            .iter()
            .map(|target| (target.path.clone(), target.format.clone()))
            .collect::<Vec<_>>();
        crate::local::metadata::write_album_batch(&file_targets, expected_revision, edit)?;
        match &self.implementation {
            Implementation::Local(local) => {
                local
                    .publish_metadata_paths(
                        database,
                        self.source_id.as_str(),
                        &paths,
                        Some(&album.object_id),
                        None,
                    )
                    .await
                    .map_err(|error| {
                        crate::SourceMetadataError::SavedRefreshFailed(error.to_string())
                    })?;
            }
            Implementation::OpenSubsonic(_) => {
                self.refresh_remote_metadata_index()
                    .await
                    .map_err(|error| {
                        crate::SourceMetadataError::SavedRefreshFailed(error.to_string())
                    })?;
                database
                    .update_album_metadata(
                        source,
                        album.album_key,
                        library::AlbumMetadataWrite {
                            title: edit.values.title.clone(),
                            normalized_title: edit.values.title.to_lowercase(),
                            display_artist: edit
                                .values
                                .album_artist
                                .clone()
                                .unwrap_or_else(|| album.display_artist.clone()),
                            sort_text: edit
                                .values
                                .sort_title
                                .clone()
                                .unwrap_or_else(|| edit.values.title.to_lowercase()),
                            year: edit.values.year.map(i64::from),
                            release_date: album.release_date.clone(),
                            date_added: album.date_added.clone(),
                            musicbrainz_release_id: edit.values.musicbrainz_album_id.clone(),
                            musicbrainz_release_group_id: edit
                                .values
                                .musicbrainz_release_group_id
                                .clone(),
                            is_compilation: album.is_compilation,
                        },
                    )
                    .await
                    .map_err(metadata_database_error)?;
            }
            Implementation::Jellyfin(_) => unreachable!(),
        }
        Ok(())
    }

    async fn write_file_artist_metadata(
        &self,
        database: &Database,
        source: library::SourceKey,
        artist: &library::ArtistRow,
        expected_revision: &str,
        edit: &crate::ArtistMetadataEdit,
    ) -> Result<(), crate::SourceMetadataError> {
        let targets = self
            .artist_metadata_targets(database, source, artist.artist_key)
            .await?;
        let paths = targets
            .iter()
            .map(|target| target.path.clone())
            .collect::<Vec<_>>();
        let file_targets = targets
            .iter()
            .map(|target| (target.path.clone(), target.format.clone()))
            .collect::<Vec<_>>();
        crate::local::metadata::write_artist_batch(
            &file_targets,
            expected_revision,
            &artist.name,
            edit,
        )?;
        match &self.implementation {
            Implementation::Local(local) => {
                local
                    .publish_metadata_paths(
                        database,
                        self.source_id.as_str(),
                        &paths,
                        None,
                        Some(&artist.object_id),
                    )
                    .await
                    .map_err(|error| {
                        crate::SourceMetadataError::SavedRefreshFailed(error.to_string())
                    })?;
            }
            Implementation::OpenSubsonic(_) => {
                self.refresh_remote_metadata_index()
                    .await
                    .map_err(|error| {
                        crate::SourceMetadataError::SavedRefreshFailed(error.to_string())
                    })?;
                database
                    .update_artist_metadata(
                        source,
                        artist.artist_key,
                        library::ArtistMetadataWrite {
                            name: edit.values.name.clone(),
                            normalized_name: edit.values.name.to_lowercase(),
                            sort_text: edit
                                .values
                                .sort_name
                                .clone()
                                .unwrap_or_else(|| edit.values.name.to_lowercase()),
                            musicbrainz_artist_id: edit.values.musicbrainz_artist_id.clone(),
                        },
                    )
                    .await
                    .map_err(metadata_database_error)?;
            }
            Implementation::Jellyfin(_) => unreachable!(),
        }
        Ok(())
    }

    async fn album_metadata_targets(
        &self,
        database: &Database,
        source: library::SourceKey,
        album: library::AlbumKey,
    ) -> Result<Vec<MetadataFileTarget>, crate::SourceMetadataError> {
        let cancel = library::ReadCancellation::new();
        let keys = database
            .album_track_order(
                source,
                album,
                None,
                "",
                library::TrackSort::TrackNumber,
                false,
                &cancel,
            )
            .await
            .map_err(metadata_database_error)?;
        self.metadata_targets(database, source, &keys).await
    }

    async fn artist_metadata_targets(
        &self,
        database: &Database,
        source: library::SourceKey,
        artist: library::ArtistKey,
    ) -> Result<Vec<MetadataFileTarget>, crate::SourceMetadataError> {
        let cancel = library::ReadCancellation::new();
        let keys = database
            .artist_track_order(
                source,
                artist,
                false,
                None,
                "",
                library::TrackSort::Title,
                false,
                &cancel,
            )
            .await
            .map_err(metadata_database_error)?;
        self.metadata_targets(database, source, &keys).await
    }

    async fn metadata_targets(
        &self,
        database: &Database,
        source: library::SourceKey,
        keys: &[library::TrackKey],
    ) -> Result<Vec<MetadataFileTarget>, crate::SourceMetadataError> {
        if keys.is_empty() {
            return Err(crate::SourceMetadataError::Unavailable);
        }
        let mut targets = Vec::with_capacity(keys.len());
        for keys in keys.chunks(128) {
            let rows = database
                .track_rows(source, keys, &library::ReadCancellation::new())
                .await
                .map_err(metadata_database_error)?;
            if rows.len() != keys.len() {
                return Err(crate::SourceMetadataError::Unavailable);
            }
            for row in rows {
                let (path, format) = self.metadata_file_target(database, source, &row).await?;
                if !crate::local::metadata_file_available(&path, format.as_deref()) {
                    return Err(crate::SourceMetadataError::Unavailable);
                }
                targets.push(MetadataFileTarget {
                    track_key: row.track_key,
                    path,
                    format,
                });
            }
        }
        targets.sort_by(|left, right| left.path.cmp(&right.path));
        targets.dedup_by(|left, right| left.path == right.path);
        Ok(targets)
    }

    async fn metadata_track_path(
        &self,
        database: &Database,
        source: library::SourceKey,
        track: &library::TrackRow,
    ) -> SourceResult<Option<(PathBuf, Option<String>)>> {
        if track.cue_path.is_some() {
            return Ok(None);
        }
        if let Some(path) = track
            .media_uri
            .as_deref()
            .and_then(|uri| uri.strip_prefix("file://"))
        {
            return Ok(Some((PathBuf::from(path), track.source_format.clone())));
        }
        if matches!(&self.implementation, Implementation::OpenSubsonic(_))
            && let Some(access) = database
                .resolve_local_access(
                    source,
                    Some(&track.object_id),
                    &track.title,
                    &track.display_album,
                    &track.display_artist,
                    track.disc_number,
                    track.track_number,
                    track.duration_millis,
                )
                .await?
                .filter(|access| access.origin == library::LocalAccessOrigin::Mapping)
        {
            return Ok(Some((
                PathBuf::from(access.path),
                track.source_format.clone(),
            )));
        }
        Ok(None)
    }

    async fn metadata_file_target(
        &self,
        database: &Database,
        source: library::SourceKey,
        track: &library::TrackRow,
    ) -> Result<(PathBuf, Option<String>), crate::SourceMetadataError> {
        if let Some(target) = self
            .metadata_track_path(database, source, track)
            .await
            .map_err(|error| crate::SourceMetadataError::Write(error.to_string()))?
        {
            return Ok(target);
        }
        if matches!(self.implementation, Implementation::OpenSubsonic(_))
            && let Some(source_path) = database
                .mapping_track_source_path(
                    source,
                    track.track_key,
                    &library::ReadCancellation::new(),
                )
                .await
                .map_err(metadata_database_error)?
        {
            return Err(crate::SourceMetadataError::LocalAccessRequired { source_path });
        }
        Err(crate::SourceMetadataError::Unavailable)
    }

    pub async fn create_playlist(
        &self,
        database: &Database,
        source: library::SourceKey,
        name: &str,
        tracks: &[library::TrackKey],
    ) -> SourceResult<(bool, Option<ScanOutcome>, Option<String>)> {
        if matches!(&self.implementation, Implementation::Local(_)) {
            let playlist = database.create_playlist(source, name, tracks).await?;
            let object_id = match playlist {
                Some(playlist) => database
                    .playlist_rows(source, &[playlist], None, &library::ReadCancellation::new())
                    .await?
                    .into_iter()
                    .next()
                    .map(|row| row.object_id),
                None => None,
            };
            return Ok((object_id.is_some(), None, object_id));
        }
        let mut pages = tracks.chunks(PROVIDER_PLAYLIST_PAGE);
        let first_ids = match pages.next() {
            Some(page) => playlist_track_ids(database, source, None, page, false).await?,
            None => Vec::new(),
        };
        let playlist = match &self.implementation {
            Implementation::Jellyfin(provider) => {
                provider.create_playlist(name, &first_ids).await?
            }
            Implementation::OpenSubsonic(provider) => {
                provider.create_playlist(name, &first_ids).await?
            }
            Implementation::Local(_) => unreachable!(),
        };
        for page in pages {
            let ids = playlist_track_ids(database, source, None, page, false).await?;
            match &self.implementation {
                Implementation::Jellyfin(provider) => {
                    provider.add_playlist_tracks(&playlist, &ids).await?
                }
                Implementation::OpenSubsonic(provider) => {
                    provider.add_playlist_tracks(&playlist, &ids).await?
                }
                Implementation::Local(_) => unreachable!(),
            }
        }
        let outcome = self
            .accept_playlist_change(database, Some(playlist.clone()), None)
            .await?;
        Ok((true, Some(outcome), Some(playlist)))
    }

    pub async fn rename_playlist(
        &self,
        database: &Database,
        source: library::SourceKey,
        playlist: library::PlaylistKey,
        name: &str,
    ) -> SourceResult<(bool, Option<ScanOutcome>)> {
        if matches!(self.implementation, Implementation::Local(_)) {
            return Ok((
                database.rename_playlist(source, playlist, name).await?,
                None,
            ));
        }
        let id = source_playlist_id(database, source, playlist).await?;
        match &self.implementation {
            Implementation::Jellyfin(provider) => provider.rename_playlist(&id, name).await?,
            Implementation::OpenSubsonic(provider) => provider.rename_playlist(&id, name).await?,
            Implementation::Local(_) => unreachable!(),
        }
        self.accept_playlist_change(database, Some(id), None)
            .await
            .map(|outcome| (true, Some(outcome)))
    }

    pub async fn delete_playlist(
        &self,
        database: &Database,
        source: library::SourceKey,
        playlist: library::PlaylistKey,
    ) -> SourceResult<(bool, Option<ScanOutcome>)> {
        if matches!(self.implementation, Implementation::Local(_)) {
            return Ok((database.delete_playlist(source, playlist).await?, None));
        }
        let id = source_playlist_id(database, source, playlist).await?;
        match &self.implementation {
            Implementation::Jellyfin(provider) => provider.delete_playlist(&id).await?,
            Implementation::OpenSubsonic(provider) => provider.delete_playlist(&id).await?,
            Implementation::Local(_) => unreachable!(),
        }
        self.accept_playlist_change(database, None, Some(id))
            .await
            .map(|outcome| (true, Some(outcome)))
    }

    pub async fn add_playlist_tracks(
        &self,
        database: &Database,
        source: library::SourceKey,
        playlist: library::PlaylistKey,
        tracks: &[library::TrackKey],
        skip_existing: bool,
    ) -> SourceResult<(bool, Option<ScanOutcome>)> {
        let skip_existing =
            skip_existing || matches!(self.implementation, Implementation::Jellyfin(_));
        if matches!(self.implementation, Implementation::Local(_)) {
            let changed = database
                .add_playlist_tracks(source, playlist, tracks, skip_existing)
                .await?
                != 0;
            return Ok((changed, None));
        }
        let id = source_playlist_id(database, source, playlist).await?;
        let mut changed = false;
        for page in tracks.chunks(PROVIDER_PLAYLIST_PAGE) {
            let ids =
                playlist_track_ids(database, source, Some(playlist), page, skip_existing).await?;
            if ids.is_empty() {
                continue;
            }
            changed = true;
            match &self.implementation {
                Implementation::Jellyfin(provider) => {
                    provider.add_playlist_tracks(&id, &ids).await?
                }
                Implementation::OpenSubsonic(provider) => {
                    provider.add_playlist_tracks(&id, &ids).await?
                }
                Implementation::Local(_) => unreachable!(),
            }
        }
        if !changed {
            return Ok((false, None));
        }
        self.accept_playlist_change(database, Some(id), None)
            .await
            .map(|outcome| (true, Some(outcome)))
    }

    pub async fn remove_playlist_entries(
        &self,
        database: &Database,
        source: library::SourceKey,
        playlist: library::PlaylistKey,
        entries: &[library::PlaylistEntryKey],
    ) -> SourceResult<(bool, Option<ScanOutcome>)> {
        if matches!(self.implementation, Implementation::Local(_)) {
            let changed = database
                .remove_playlist_entries(source, playlist, entries)
                .await?
                != 0;
            return Ok((changed, None));
        }
        let id = source_playlist_id(database, source, playlist).await?;
        for page in entries.chunks(PROVIDER_PLAYLIST_PAGE) {
            let occurrences = database
                .source_playlist_entry_object_ids(
                    source,
                    playlist,
                    page,
                    &library::ReadCancellation::new(),
                )
                .await?;
            if occurrences.len() != page.len() {
                return Err(SourceError::InvalidRequest(
                    "Playlist entry is no longer current",
                ));
            }
            match &self.implementation {
                Implementation::Jellyfin(provider) => {
                    provider.remove_playlist_entries(&id, &occurrences).await?
                }
                Implementation::OpenSubsonic(provider) => {
                    provider.remove_playlist_entries(&id, &occurrences).await?
                }
                Implementation::Local(_) => unreachable!(),
            }
        }
        self.accept_playlist_change(database, Some(id), None)
            .await
            .map(|outcome| (true, Some(outcome)))
    }

    pub async fn move_playlist_entry(
        &self,
        database: &Database,
        source: library::SourceKey,
        playlist: library::PlaylistKey,
        entry: library::PlaylistEntryKey,
        position: usize,
    ) -> SourceResult<(bool, Option<ScanOutcome>)> {
        if matches!(self.implementation, Implementation::Local(_)) {
            let changed = database
                .move_playlist_entry(source, playlist, entry, position)
                .await?;
            return Ok((changed, None));
        }
        let id = source_playlist_id(database, source, playlist).await?;
        let occurrence = database
            .source_playlist_entry_object_ids(
                source,
                playlist,
                &[entry],
                &library::ReadCancellation::new(),
            )
            .await?
            .pop()
            .ok_or(SourceError::InvalidRequest(
                "Playlist entry is no longer current",
            ))?;
        match &self.implementation {
            Implementation::Jellyfin(provider) => {
                provider
                    .move_playlist_entry(&id, &occurrence, position)
                    .await?
            }
            Implementation::OpenSubsonic(provider) => {
                provider
                    .move_playlist_entry(&id, &occurrence, position)
                    .await?
            }
            Implementation::Local(_) => unreachable!(),
        }
        self.accept_playlist_change(database, Some(id), None)
            .await
            .map(|outcome| (true, Some(outcome)))
    }

    async fn accept_playlist_change(
        &self,
        database: &Database,
        affected: Option<String>,
        deleted: Option<String>,
    ) -> SourceResult<ScanOutcome> {
        match &self.implementation {
            Implementation::Jellyfin(source) => {
                source
                    .apply_live_items(
                        database,
                        self.source_id.as_str(),
                        affected
                            .into_iter()
                            .map(|id| raw_item_id(&id).to_string())
                            .collect(),
                        deleted
                            .into_iter()
                            .map(|id| raw_item_id(&id).to_string())
                            .collect(),
                    )
                    .await
            }
            Implementation::OpenSubsonic(source) => {
                let mut scan = Scan::begin_items(database, self.source_id.as_str()).await?;
                if let Some(playlist) = affected {
                    source.stage_playlist_snapshot(&mut scan, &playlist).await?;
                } else if let Some(playlist) = deleted {
                    scan.begin_batch().await?;
                    scan.remove_playlist(&playlist).await?;
                    scan.finish_batch().await?;
                }
                Ok(scan.finish().await?)
            }
            Implementation::Local(_) => Err(SourceError::InvalidRequest(
                "Local playlists do not have provider changes",
            )),
        }
    }

    pub async fn lyrics(&self, track_object_id: &str) -> SourceResult<Option<crate::NativeLyrics>> {
        match &self.implementation {
            Implementation::Local(_) => Ok(None),
            Implementation::Jellyfin(source) => source.lyrics(track_object_id).await,
            Implementation::OpenSubsonic(source) => source.lyrics(track_object_id).await,
        }
    }

    pub async fn browse_folder(
        &self,
        folder_object_id: Option<&str>,
        music_folder_object_id: Option<&str>,
    ) -> SourceResult<LiveFolderPage> {
        match &self.implementation {
            Implementation::Local(_) => Err(SourceError::InvalidRequest(
                "Local Folder browsing is Database-owned",
            )),
            Implementation::Jellyfin(source) => {
                source
                    .browse_folder(folder_object_id, music_folder_object_id)
                    .await
            }
            Implementation::OpenSubsonic(source) => {
                source
                    .browse_folder(folder_object_id, music_folder_object_id)
                    .await
            }
        }
    }

    pub async fn generated_track_object_ids(
        &self,
        seed: &SourceRadioSeed,
        limit: usize,
    ) -> SourceResult<Vec<String>> {
        match &self.implementation {
            Implementation::Local(_) => Ok(Vec::new()),
            Implementation::Jellyfin(source) => {
                source.generated_track_object_ids(seed, limit).await
            }
            Implementation::OpenSubsonic(source) => {
                source.generated_track_object_ids(seed, limit).await
            }
        }
    }

    pub async fn collection_track_object_ids(
        &self,
        collection: &SourceCollection,
        limit: usize,
    ) -> SourceResult<Vec<String>> {
        match &self.implementation {
            Implementation::Local(_) => Ok(Vec::new()),
            Implementation::Jellyfin(source) => {
                source.collection_track_object_ids(collection, limit).await
            }
            Implementation::OpenSubsonic(source) => {
                source.collection_track_object_ids(collection, limit).await
            }
        }
    }

    pub async fn home_section(
        &self,
        section: SourceHomeSection,
    ) -> SourceResult<Vec<library::HomeEntryInput>> {
        match &self.implementation {
            Implementation::Local(_) => Ok(Vec::new()),
            Implementation::Jellyfin(source) => source.home_section(section).await,
            Implementation::OpenSubsonic(source) => source.home_section(section).await,
        }
    }

    pub fn start_selected_feed(
        self: &Arc<Self>,
        runtime: &tokio::runtime::Handle,
        database: Arc<Database>,
        source_key: library::SourceKey,
    ) -> Option<Arc<SelectedFeed>> {
        if matches!(self.implementation, Implementation::OpenSubsonic(_)) {
            return None;
        }
        let (wake, receiver) = async_channel::bounded(1);
        let feed = Arc::new(SelectedFeed {
            source: Arc::clone(self),
            database,
            source_key,
            pending: Mutex::new(None),
            wake,
            cancelled: Arc::new(AtomicBool::new(false)),
            receiver,
        });
        match &self.implementation {
            Implementation::Local(_) => {
                let producer = Arc::clone(&feed);
                let _ = std::thread::Builder::new()
                    .name("rufin-local-watcher".to_string())
                    .spawn(move || {
                        let ready_feed = Arc::clone(&producer);
                        let mut ready = move |recovered: bool| {
                            (!recovered
                                || ready_feed
                                    .submit(SelectedFeedChange::Local(LocalLiveChange::Rescan)))
                                && !ready_feed.cancelled.load(Ordering::Acquire)
                        };
                        let change_feed = Arc::clone(&producer);
                        let mut changed =
                            move |change| change_feed.submit(SelectedFeedChange::Local(change));
                        let stop_feed = Arc::clone(&producer);
                        if let Implementation::Local(local) = &producer.source.implementation {
                            let _ = local.watch(&mut ready, &mut changed, &|| {
                                stop_feed.cancelled.load(Ordering::Acquire)
                            });
                        }
                    });
            }
            Implementation::Jellyfin(_) => {
                let producer = Arc::clone(&feed);
                runtime.spawn(async move {
                    let ready_feed = Arc::clone(&producer);
                    let mut ready = move || !ready_feed.cancelled.load(Ordering::Acquire);
                    let gap_feed = Arc::clone(&producer);
                    let mut gap = move || {
                        gap_feed.submit(SelectedFeedChange::Jellyfin(
                            JellyfinLiveChange::BoundaryLost,
                        ))
                    };
                    let change_feed = Arc::clone(&producer);
                    let mut changed =
                        move |change| change_feed.submit(SelectedFeedChange::Jellyfin(change));
                    let result = match &producer.source.implementation {
                        Implementation::Jellyfin(provider) => {
                            provider
                                .listen_library_changes(&mut ready, &mut gap, &mut changed)
                                .await
                        }
                        _ => return,
                    };
                    if producer.cancelled.load(Ordering::Acquire) {
                        return;
                    }
                    if let Err(error) = result {
                        tracing::warn!(%error, "Jellyfin library change feed stopped");
                    }
                    producer.submit(SelectedFeedChange::Jellyfin(
                        JellyfinLiveChange::BoundaryLost,
                    ));
                });
            }
            Implementation::OpenSubsonic(_) => unreachable!(),
        }
        Some(feed)
    }

    #[cfg(test)]
    pub(crate) async fn apply_local_change(
        &self,
        database: &Database,
        source: library::SourceKey,
        change: LocalLiveChange,
    ) -> SourceResult<Option<ScanOutcome>> {
        match (&self.implementation, change) {
            (Implementation::Local(local), LocalLiveChange::Paths { paths, rename }) => local
                .publish_paths(
                    database,
                    source,
                    self.source_id.as_str(),
                    &paths,
                    rename.as_ref(),
                )
                .await
                .map(Some),
            (Implementation::Local(_), LocalLiveChange::Rescan) => Ok(None),
            _ => Err(SourceError::InvalidRequest(
                "Local change belongs to another source",
            )),
        }
    }

    pub async fn prepare_local_artwork_page(
        &self,
        database: &Database,
        source: library::SourceKey,
        after: Option<library::AlbumKey>,
    ) -> SourceResult<ArtworkPreparationProgress> {
        match &self.implementation {
            Implementation::Local(local) => {
                local.prepare_artwork_page(database, source, after).await
            }
            _ => Ok(ArtworkPreparationProgress {
                completed: 0,
                next_album: None,
            }),
        }
    }

    pub async fn report_playback(&self, report: &SourceReportFact) -> SourceResult<()> {
        match &self.implementation {
            Implementation::Local(_) => Ok(()),
            Implementation::Jellyfin(source) => source.report_playback(report).await,
            Implementation::OpenSubsonic(source) => source.report_playback(report).await,
        }
    }

    pub async fn apply_local_mapping(
        &self,
        database: &Database,
        source: library::SourceKey,
        root: &std::path::Path,
        server_prefix: Option<&str>,
        local_prefix: Option<&str>,
        sample_source_path: Option<&str>,
        cancelled: Arc<AtomicBool>,
    ) -> SourceResult<usize> {
        let root =
            std::fs::canonicalize(root).map_err(|error| SourceError::Other(error.to_string()))?;
        if cancelled.load(Ordering::Acquire) {
            return Err(SourceError::Cancelled);
        }
        let sample = database
            .mapping_track_page(
                source,
                None,
                sample_source_path,
                1,
                &library::ReadCancellation::new(),
            )
            .await?
            .pop()
            .ok_or(SourceError::InvalidRequest(
                "No representative Track matches this mapping",
            ))?;
        let access = mapped_track_access(&root, server_prefix, local_prefix, sample)?.ok_or(
            SourceError::InvalidRequest("The representative Track does not map to a local file"),
        )?;
        database.clear_mapping_access(source).await?;
        database.upsert_local_access(source, &access).await?;
        Ok(1)
    }

    pub async fn complete_local_mapping(
        &self,
        database: &Database,
        source: library::SourceKey,
        root: &std::path::Path,
        server_prefix: Option<&str>,
        local_prefix: Option<&str>,
        cancelled: Arc<AtomicBool>,
    ) -> SourceResult<usize> {
        let root =
            std::fs::canonicalize(root).map_err(|error| SourceError::Other(error.to_string()))?;
        let mut after = None;
        let mut accepted = 0;
        loop {
            if cancelled.load(Ordering::Acquire) {
                return Err(SourceError::Cancelled);
            }
            let tracks = database
                .mapping_track_page(source, after, None, 128, &library::ReadCancellation::new())
                .await?;
            if tracks.is_empty() {
                break;
            }
            after = tracks.last().map(|track| track.track_key);
            for track in tracks {
                if cancelled.load(Ordering::Acquire) {
                    return Err(SourceError::Cancelled);
                }
                if let Some(access) =
                    mapped_track_access(&root, server_prefix, local_prefix, track)?
                {
                    database.upsert_local_access(source, &access).await?;
                    accepted += 1;
                }
            }
        }
        accepted += crate::local::apply_metadata_mapping(database, source, &root, &|| {
            cancelled.load(Ordering::Acquire)
        })
        .await?;
        Ok(accepted)
    }
}

fn mapped_track_access(
    root: &std::path::Path,
    server_prefix: Option<&str>,
    local_prefix: Option<&str>,
    track: library::MappingTrackRow,
) -> SourceResult<Option<library::LocalAccessWrite>> {
    let Some(path) = mapped_local_path(root, server_prefix, local_prefix, &track.source_path)
    else {
        return Ok(None);
    };
    let metadata =
        std::fs::metadata(&path).map_err(|error| SourceError::Other(error.to_string()))?;
    let mtime_ns = metadata
        .modified()
        .ok()
        .and_then(|value| value.duration_since(std::time::UNIX_EPOCH).ok())
        .and_then(|value| i64::try_from(value.as_nanos()).ok())
        .unwrap_or_default();
    #[cfg(unix)]
    let (device_id, inode) = {
        use std::os::unix::fs::MetadataExt;
        (
            i64::try_from(metadata.dev()).ok(),
            i64::try_from(metadata.ino()).ok(),
        )
    };
    #[cfg(not(unix))]
    let (device_id, inode) = (None, None);
    let media_uri = url::Url::from_file_path(&path)
        .map_err(|()| SourceError::Other("could not create mapped file URI".to_string()))?
        .to_string();
    let mut hash = blake3::Hasher::new();
    hash.update(path.to_string_lossy().as_bytes());
    hash.update(&metadata.len().to_le_bytes());
    hash.update(&mtime_ns.to_le_bytes());
    Ok(Some(library::LocalAccessWrite {
        track_object_id: Some(track.object_id),
        origin: library::LocalAccessOrigin::Mapping,
        path: path.to_string_lossy().into_owned(),
        root: root.to_string_lossy().into_owned(),
        relative_path: path
            .strip_prefix(root)
            .unwrap_or(&path)
            .to_string_lossy()
            .into_owned(),
        size_bytes: i64::try_from(metadata.len()).unwrap_or(i64::MAX),
        mtime_ns,
        device_id,
        inode,
        parser_version: 1,
        title: track.title,
        album: track.album,
        artist: track.artist,
        disc_number: track.disc_number,
        track_number: track.track_number,
        duration_millis: track.duration_millis,
        media_uri,
        loudness_analysis_key: *hash.finalize().as_bytes(),
    }))
}

fn mapped_local_path(
    root: &std::path::Path,
    server_prefix: Option<&str>,
    local_prefix: Option<&str>,
    source_path: &str,
) -> Option<PathBuf> {
    let mut base = root.to_path_buf();
    if let Some(prefix) = local_prefix.filter(|prefix| !prefix.trim().is_empty()) {
        base.push(prefix);
    }
    if let Some(prefix) = server_prefix {
        let relative = source_path
            .strip_prefix(prefix)?
            .trim_start_matches(['/', '\\']);
        return mapped_file(root, base.join(relative.replace('\\', "/")));
    }
    if let Some(path) = mapped_file(root, PathBuf::from(source_path)) {
        return Some(path);
    }
    let components = std::path::Path::new(source_path)
        .components()
        .filter_map(|component| match component {
            std::path::Component::Normal(value) => Some(value),
            _ => None,
        })
        .collect::<Vec<_>>();
    (0..components.len()).find_map(|start| {
        let path = components[start..]
            .iter()
            .fold(base.clone(), |mut path, part| {
                path.push(part);
                path
            });
        mapped_file(root, path)
    })
}

fn mapped_file(root: &std::path::Path, candidate: PathBuf) -> Option<PathBuf> {
    let path = std::fs::canonicalize(candidate).ok()?;
    (path.starts_with(root) && path.is_file()).then_some(path)
}

struct MetadataFileTarget {
    track_key: library::TrackKey,
    path: PathBuf,
    format: Option<String>,
}

fn album_metadata_from_targets(
    album: library::AlbumRow,
    targets: &[MetadataFileTarget],
) -> Result<crate::AlbumMetadata, crate::SourceMetadataError> {
    let first = targets
        .first()
        .ok_or(crate::SourceMetadataError::Unavailable)?;
    let track =
        crate::local::read_track_metadata(first.track_key, &first.path, first.format.as_deref())?;
    let mut values =
        crate::local::read_album_metadata_values(&first.path, first.format.as_deref())?;
    if values.title.is_empty() {
        values.title = album.title.clone();
    }
    if values.album_artist.is_none() {
        values.album_artist = Some(album.display_artist.clone());
    }
    if values.artist.is_none() {
        values.artist = Some(album.display_artist.clone());
    }
    let source_values = values.clone();
    let mut rufin_filled = crate::AlbumMetadataWritable::default();
    if values.musicbrainz_album_id.is_none() && album.musicbrainz_release_id.is_some() {
        values.musicbrainz_album_id = album.musicbrainz_release_id.clone();
        rufin_filled.musicbrainz_album_id = true;
    }
    if values.musicbrainz_release_group_id.is_none() && album.musicbrainz_release_group_id.is_some()
    {
        values.musicbrainz_release_group_id = album.musicbrainz_release_group_id.clone();
        rufin_filled.musicbrainz_release_group_id = true;
    }
    let mut mixed = crate::AlbumMetadataMixed::default();
    for target in &targets[1..] {
        let observed =
            crate::local::read_album_metadata_values(&target.path, target.format.as_deref())?;
        mixed.title |= observed.title != source_values.title;
        mixed.sort_title |= observed.sort_title != source_values.sort_title;
        mixed.artist |= observed.artist != source_values.artist;
        mixed.album_artist |= observed.album_artist != source_values.album_artist;
        mixed.year |= observed.year != source_values.year;
        mixed.genre |= observed.genre != source_values.genre;
        mixed.comment |= observed.comment != source_values.comment;
        mixed.musicbrainz_album_id |=
            observed.musicbrainz_album_id != source_values.musicbrainz_album_id;
        mixed.musicbrainz_release_group_id |=
            observed.musicbrainz_release_group_id != source_values.musicbrainz_release_group_id;
    }
    let paths = targets
        .iter()
        .map(|target| target.path.clone())
        .collect::<Vec<_>>();
    Ok(crate::AlbumMetadata {
        album_key: album.album_key,
        writable: crate::AlbumMetadataWritable {
            title: track.writable.album,
            sort_title: track.writable.sort_title,
            artist: track.writable.artist,
            album_artist: track.writable.album_artist,
            year: track.writable.year,
            genre: track.writable.genre,
            comment: track.writable.comment,
            locked: false,
            musicbrainz_album_id: track.writable.musicbrainz_album_id,
            musicbrainz_release_group_id: track.writable.musicbrainz_release_group_id,
        },
        source_search: false,
        revision: Some(crate::local::metadata::combined_revision(&paths)?),
        source_values,
        values,
        rufin_filled,
        track_count: targets.len(),
        mixed,
    })
}

fn artist_metadata_from_targets(
    artist: library::ArtistRow,
    targets: &[MetadataFileTarget],
) -> Result<crate::ArtistMetadata, crate::SourceMetadataError> {
    let first = targets
        .first()
        .ok_or(crate::SourceMetadataError::Unavailable)?;
    let track =
        crate::local::read_track_metadata(first.track_key, &first.path, first.format.as_deref())?;
    let mut values = crate::local::read_artist_metadata_values(
        &first.path,
        first.format.as_deref(),
        &artist.name,
    )?;
    let source_values = values.clone();
    let mut rufin_filled = crate::ArtistMetadataWritable::default();
    if values.musicbrainz_artist_id.is_none() && artist.musicbrainz_artist_id.is_some() {
        values.musicbrainz_artist_id = artist.musicbrainz_artist_id.clone();
        rufin_filled.musicbrainz_artist_id = true;
    }
    let mut mixed = crate::ArtistMetadataMixed::default();
    for target in &targets[1..] {
        let observed = crate::local::read_artist_metadata_values(
            &target.path,
            target.format.as_deref(),
            &artist.name,
        )?;
        mixed.sort_name |= observed.sort_name != source_values.sort_name;
        mixed.genre |= observed.genre != source_values.genre;
        mixed.comment |= observed.comment != source_values.comment;
        mixed.musicbrainz_artist_id |=
            observed.musicbrainz_artist_id != source_values.musicbrainz_artist_id;
    }
    let paths = targets
        .iter()
        .map(|target| target.path.clone())
        .collect::<Vec<_>>();
    Ok(crate::ArtistMetadata {
        artist_key: artist.artist_key,
        writable: crate::ArtistMetadataWritable {
            name: track.writable.artist,
            sort_name: track.writable.sort_title,
            genre: track.writable.genre,
            comment: track.writable.comment,
            locked: false,
            musicbrainz_artist_id: track.writable.musicbrainz_artist_id,
        },
        source_search: false,
        revision: Some(crate::local::metadata::combined_revision(&paths)?),
        source_values,
        values,
        rufin_filled,
        track_count: targets.len(),
        mixed,
    })
}

fn track_write_from_values(
    track: &library::TrackRow,
    values: &crate::TrackMetadataValues,
) -> library::TrackMetadataWrite {
    let title = values.title.trim().to_string();
    let album = values
        .album
        .clone()
        .unwrap_or_else(|| track.display_album.clone());
    let artist = values
        .artist
        .clone()
        .unwrap_or_else(|| track.display_artist.clone());
    library::TrackMetadataWrite {
        normalized_search: format!(
            "{title} {album} {artist} {}",
            values.comment.as_deref().unwrap_or_default()
        )
        .to_lowercase(),
        sort_text: values
            .sort_title
            .clone()
            .unwrap_or_else(|| title.to_lowercase()),
        title,
        display_album: album,
        display_artist: artist,
        duration_millis: track.duration_millis,
        disc_number: values
            .disc_number
            .map(i64::from)
            .unwrap_or(track.disc_number),
        track_number: values
            .track_number
            .map(i64::from)
            .unwrap_or(track.track_number),
        year: values.year.map(i64::from),
        release_date: track.release_date.clone(),
        date_added: track.date_added.clone(),
        media_uri: track.media_uri.clone(),
        source_format: track.source_format.clone(),
        comment: values.comment.clone(),
        bpm: values.bpm.map(i64::from),
        musicbrainz_recording_id: values.musicbrainz_recording_id.clone(),
        musicbrainz_release_track_id: values.musicbrainz_release_track_id.clone(),
        cue_path: track.cue_path.clone(),
        cue_start_millis: track.cue_start_millis,
        cue_end_millis: track.cue_end_millis,
        loudness_analysis_key: track.loudness_analysis_key,
    }
}

#[cfg(test)]
mod live_acquisition_tests {
    use super::*;

    #[tokio::test]
    async fn local_search_and_folder_are_unavailable_not_successfully_empty() {
        let directory = tempfile::tempdir().unwrap();
        let local =
            crate::local::LocalSource::from_roots(vec![directory.path().to_path_buf()]).unwrap();
        let source = Source::new(
            SourceId::new(crate::local::LOCAL_LIBRARY_SOURCE_ID),
            Implementation::Local(local),
        );
        assert!(matches!(
            source.live_search("track", 20).await,
            Err(SourceError::InvalidRequest(_))
        ));
        assert!(matches!(
            source.browse_folder(None, None).await,
            Err(SourceError::InvalidRequest(_))
        ));
    }
}

fn metadata_database_error(error: library::LibraryError) -> crate::SourceMetadataError {
    crate::SourceMetadataError::Write(error.to_string())
}

async fn source_playlist_id(
    database: &Database,
    source: library::SourceKey,
    playlist: library::PlaylistKey,
) -> SourceResult<String> {
    database
        .source_playlist_object_id(source, playlist, &library::ReadCancellation::new())
        .await?
        .ok_or(SourceError::InvalidRequest("Playlist is no longer current"))
}

async fn playlist_track_ids(
    database: &Database,
    source: library::SourceKey,
    playlist: Option<library::PlaylistKey>,
    tracks: &[library::TrackKey],
    skip_existing: bool,
) -> SourceResult<Vec<String>> {
    let ids = database
        .source_playlist_track_object_ids(
            source,
            playlist,
            tracks,
            skip_existing,
            &library::ReadCancellation::new(),
        )
        .await?;
    if !skip_existing && ids.len() != tracks.len() {
        return Err(SourceError::InvalidRequest(
            "Playlist Track is no longer current",
        ));
    }
    Ok(ids)
}

pub(crate) fn require_source_edit(
    current: &SourceConfiguration,
    expected_kind: &str,
) -> SourceResult<()> {
    if current.kind == expected_kind {
        Ok(())
    } else {
        Err(SourceError::InvalidConfig(format!(
            "expected {expected_kind}, found {}",
            current.kind
        )))
    }
}

pub(crate) fn configured_source_name(configured: Option<String>, provider: String) -> String {
    configured
        .filter(|name| !name.trim().is_empty())
        .map(|name| name.trim().to_string())
        .unwrap_or(provider)
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

#[cfg(test)]
mod refresh_laws {
    use super::*;

    #[tokio::test]
    async fn unsupported_freshness_never_widens_to_a_catalog_acquisition() {
        let directory = tempfile::tempdir().expect("Store directory");
        let database = Database::open(directory.path().join("library.sqlite"))
            .await
            .expect("Database");
        let source = Source::open(
            SourceConfiguration {
                source_id: SourceId::new("jellyfin:test"),
                kind: "jellyfin".to_string(),
                name: "Jellyfin".to_string(),
                provider_payload: serde_json::json!({
                    "version": 1,
                    "base_url": "http://127.0.0.1:9",
                    "server_id": "test",
                    "user_id": "user",
                    "username": "listener",
                    "trust_invalid_cert": false,
                    "use_jellyfin_instant_mix": false
                })
                .to_string(),
            },
            Some("token".to_string()),
            Some("device".to_string()),
        )
        .expect("open Jellyfin source");

        assert_eq!(
            source
                .refresh_if_needed(
                    &database,
                    "Jellyfin",
                    &|_| panic!("unsupported freshness started acquisition progress"),
                    Arc::new(AtomicBool::new(false)),
                )
                .await
                .expect("freshness check"),
            None
        );
        assert!(
            database
                .cached_source("jellyfin:test", &library::ReadCancellation::new())
                .await
                .expect("cached source")
                .is_none()
        );
    }

    #[test]
    fn selected_feed_accumulates_exact_jellyfin_ids_with_one_bound() {
        let merged = SelectedFeedChange::Jellyfin(JellyfinLiveChange::Items {
            upserts: vec!["two".to_string(), "one".to_string()],
            removals: Vec::new(),
        })
        .merge(SelectedFeedChange::Jellyfin(JellyfinLiveChange::Items {
            upserts: vec!["one".to_string(), "three".to_string()],
            removals: vec!["gone".to_string()],
        }));
        let SelectedFeedChange::Jellyfin(JellyfinLiveChange::Items { upserts, removals }) = merged
        else {
            panic!("exact evidence widened unexpectedly");
        };
        assert_eq!(upserts, ["one", "three", "two"]);
        assert_eq!(removals, ["gone"]);
    }

    #[test]
    fn selected_feed_overflow_coalesces_to_one_boundary_operation() {
        let merged = SelectedFeedChange::Jellyfin(JellyfinLiveChange::Items {
            upserts: (0..LIVE_CHANGE_LIMIT)
                .map(|index| format!("item-{index}"))
                .collect(),
            removals: Vec::new(),
        })
        .merge(SelectedFeedChange::Jellyfin(JellyfinLiveChange::Items {
            upserts: vec!["overflow".to_string()],
            removals: Vec::new(),
        }));
        assert!(matches!(
            merged,
            SelectedFeedChange::Jellyfin(JellyfinLiveChange::BoundaryLost)
        ));
    }

    #[test]
    fn repeated_boundary_evidence_admits_one_recovery() {
        let (wake, receiver) = async_channel::bounded(1);
        let mut pending = Some(SelectedFeedChange::Jellyfin(
            JellyfinLiveChange::BoundaryLost,
        ));
        pending = Some(pending.take().expect("first boundary").merge(
            SelectedFeedChange::Jellyfin(JellyfinLiveChange::BoundaryLost),
        ));
        assert!(wake.try_send(()).is_ok());
        let _ = wake.try_send(());
        assert!(matches!(
            pending.as_ref(),
            Some(SelectedFeedChange::Jellyfin(
                JellyfinLiveChange::BoundaryLost
            ))
        ));
        assert!(receiver.try_recv().is_ok());
        assert!(receiver.try_recv().is_err());
    }

    #[test]
    fn representative_mapping_finds_the_existing_suffix_without_a_manual_server_prefix() {
        let root = tempfile::tempdir().expect("local music root");
        let track = root.path().join("Artist").join("Album").join("Track.flac");
        std::fs::create_dir_all(track.parent().expect("Track parent")).expect("create Album");
        std::fs::write(&track, b"media").expect("write Track");
        let canonical_root = root.path().canonicalize().expect("canonical music root");

        assert_eq!(
            mapped_local_path(
                &canonical_root,
                None,
                None,
                "/srv/navidrome/music/Artist/Album/Track.flac",
            ),
            Some(track.canonicalize().expect("canonical Track"))
        );
    }
}
