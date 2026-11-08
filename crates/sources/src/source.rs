//! Provider operations write canonical catalog data directly.
//! Source owns native client lifetimes and publishes committed changes.

use std::collections::{BTreeMap, HashSet};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use crate::{DownloadQuality, ResolvedDownload};
use async_channel::{Receiver, Sender};
use audio_processing::ResolvedStream;
use library::{Catalog, Freshness};
use playback::{SourceReportFact, StreamRequest};
use serde::{Deserialize, Serialize};

use crate::policy::raw_item_id;
pub(crate) mod metadata;
use crate::{
    ImageBytes, SourceConfiguration, SourceError, SourceId, SourceResult, SourceSettingsChange,
    SourceSetup,
};
pub use metadata::FileEditSession;

/// A missing file differs from a path that could not be checked.
pub async fn local_files_available(
    paths: Vec<Vec<library::LocalMediaLocation>>,
) -> SourceResult<Vec<bool>> {
    tokio::task::spawn_blocking(move || {
        paths
            .into_iter()
            .map(|paths| {
                let mut failure = None;
                for path in paths {
                    let available = match path {
                        library::LocalMediaLocation::File(path) => match std::fs::metadata(path) {
                            Ok(metadata) => Ok(metadata.is_file()),
                            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
                            Err(error) => Err(SourceError::Other(error.to_string())),
                        },
                        library::LocalMediaLocation::Document(uri) => {
                            match crate::stat_document(&uri) {
                                Ok(entry) => Ok(!entry.directory),
                                Err(SourceError::NotFound) => Ok(false),
                                Err(error) => Err(error),
                            }
                        }
                    };
                    match available {
                        Ok(true) => return Ok(true),
                        Ok(false) => {}
                        Err(error) => failure = Some(error),
                    }
                }
                match failure {
                    Some(error) => Err(error),
                    None => Ok(false),
                }
            })
            .collect()
    })
    .await
    .map_err(|error| SourceError::Other(error.to_string()))?
}

/// Lyrics beside or inside a local file: the playable's download or local file, else the file
/// its URI names.
pub async fn read_local_lyrics(
    catalog: &Catalog,
    media_uri: &str,
    playable: Option<&library::Playable>,
    title: Option<&str>,
    preferences: &lyrics::UserPreferences,
) -> SourceResult<Option<lyrics::LyricsBundle>> {
    let local_path = |uri: &str| {
        library::cue_media_parts(uri).map_or_else(
            || library::file_media_path(uri),
            |(_, uri, _, _)| library::file_media_path(&uri),
        )
    };
    let access = playable.and_then(|playable| {
        playable
            .download
            .as_ref()
            .map(|download| download.uri.clone())
            .or_else(|| playable.local_uri())
    });
    let mapped = match playable.and_then(|playable| playable.mapped.clone()) {
        Some(mapped) => tokio::task::spawn_blocking(move || mapped.locate())
            .await
            .map_err(|error| SourceError::Other(error.to_string()))?,
        None => None,
    };
    let mapped = match mapped {
        Some(library::LocalMediaLocation::File(path)) => Some(path),
        _ => None,
    };
    let Some(path) = access
        .as_deref()
        .and_then(local_path)
        .or(mapped)
        .or_else(|| local_path(media_uri))
    else {
        return Ok(None);
    };
    let cue_track = library::cue_media_parts(media_uri).is_some();
    let title = match title {
        Some(title) => title.to_string(),
        None => catalog
            .track(&(media_uri), &library::ReadCancellation::new())
            .await?
            .map(|row| row.metadata.title)
            .unwrap_or_else(|| {
                if cue_track {
                    String::new()
                } else {
                    path.file_stem()
                        .unwrap_or_default()
                        .to_string_lossy()
                        .into_owned()
                }
            }),
    };
    let input = lyrics::LocalLyricsInput {
        audio_path: path,
        title,
        cue_track,
    };
    let preferences = preferences.clone();
    tokio::task::spawn_blocking(move || {
        lyrics::local_file_lyrics(&input, &preferences, || {
            crate::file::metadata::read_embedded_lyrics(&input.audio_path)
                .ok()
                .flatten()
                .and_then(|text| lyrics::lyrics_from_edited_text(&text))
        })
    })
    .await
    .map_err(|error| SourceError::Other(error.to_string()))
}

pub(crate) async fn cache_native_lyrics(
    catalog: &Catalog,
    uri: &str,
    bundle: &mut Option<lyrics::LyricsBundle>,
) -> SourceResult<()> {
    *bundle = bundle
        .take()
        .and_then(lyrics::lyrics_with_displayable_content);
    let Some(bundle) = bundle else {
        catalog
            .remove_track_lyrics_by_authority(uri, "source")
            .await?;
        return Ok(());
    };
    bundle.origin = lyrics::LyricsOrigin::Native;
    let payload = bundle.cache_payload()?;
    catalog
        .write_lyrics_cache(
            uri,
            "source",
            "original",
            "",
            "",
            &payload,
            unix_now(),
            false,
        )
        .await?;
    Ok(())
}

pub(crate) const LIVE_CHANGE_LIMIT: usize = 128;
/// How often a live feed pings its server, and how long it waits for any answer before it
/// takes the connection for gone, as after a network change.
pub(crate) const FEED_PING_INTERVAL: std::time::Duration = std::time::Duration::from_secs(30);
pub(crate) const FEED_SILENCE_LIMIT: std::time::Duration = std::time::Duration::from_secs(75);
/// A catch-up asks for changes from this long before the last sync, so a save the server was
/// still writing and a small clock skew are covered.
pub(crate) const SYNC_MARGIN: i64 = 60;

/// What a catalog refresh lists.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Refresh {
    /// The periodic check and a lost feed boundary: freshness first, a listing only when it
    /// changed; a server with dated changes lists what changed since the last sync.
    Check,
    /// A listing of what changed.
    Changed,
    /// A listing that also reads rejected files again (Resync Library).
    Resync,
}

pub(crate) fn unix_now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |time| time.as_secs().min(i64::MAX as u64) as i64)
}

pub struct SelectedFeed {
    source: Arc<Source>,
    pending: Mutex<Option<SelectedFeedChange>>,
    wake: Sender<()>,
    cancelled: Arc<AtomicBool>,
    remote_task: Mutex<Option<tokio::task::AbortHandle>>,
    receiver: Receiver<()>,
}

enum SelectedFeedChange {
    Remote(RemoteItemChange),
    Files(crate::file::remote::changes::FileChange),
    Local(LocalLiveChange),
}

impl SelectedFeed {
    fn submit(&self, incoming: SelectedFeedChange) -> bool {
        if self.cancelled.load(Ordering::Acquire) {
            return false;
        }
        self.keep(incoming);
        let _ = self.wake.try_send(());
        true
    }

    fn keep(&self, incoming: SelectedFeedChange) {
        let mut pending = self
            .pending
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        *pending = Some(match pending.take() {
            Some(current) => current.merge(incoming),
            None => incoming,
        });
    }
}

impl SelectedFeedChange {
    fn lists(&self) -> bool {
        matches!(
            self,
            Self::Remote(RemoteItemChange::BoundaryLost)
                | Self::Files(crate::file::remote::changes::FileChange::FullRefresh)
                | Self::Local(LocalLiveChange::Rescan)
        )
    }

    fn merge(self, incoming: Self) -> Self {
        match (self, incoming) {
            (Self::Remote(current), Self::Remote(incoming)) => {
                Self::Remote(current.merge(incoming))
            }
            (Self::Files(current), Self::Files(incoming)) => Self::Files(current.merge(incoming)),
            (Self::Local(current), Self::Local(incoming)) => Self::Local(current.merge(incoming)),
            (_, incoming) => incoming,
        }
    }
}

impl SelectedFeed {
    pub fn cancel(&self) {
        self.cancelled.store(true, Ordering::Release);
        if let Some(task) = self
            .remote_task
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .take()
        {
            task.abort();
        }
        let _ = self.wake.try_send(());
    }

    pub fn wake_pending(&self) {
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

    /// Whether the pending change lists the whole source.
    pub fn pending_listing(&self) -> bool {
        self.pending
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .as_ref()
            .is_some_and(SelectedFeedChange::lists)
    }

    /// Applies the pending change. A change that lists the whole source runs through `list`,
    /// which says whether the listing ran; one that did not stays pending for the next check.
    pub async fn apply_pending(
        &self,
        catalog: &Catalog,
        list: impl std::ops::AsyncFnOnce(Refresh) -> bool,
    ) -> SourceResult<()> {
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
        match change {
            SelectedFeedChange::Remote(RemoteItemChange::Items(items)) => {
                let result = self.source.apply_changes(catalog, items).await;
                // What a failed apply missed comes from a listing of what changed.
                if result
                    .as_ref()
                    .is_err_and(|error| !matches!(error, SourceError::Cancelled))
                {
                    self.submit(SelectedFeedChange::Remote(RemoteItemChange::BoundaryLost));
                }
                result
            }
            SelectedFeedChange::Files(crate::file::remote::changes::FileChange::Paths {
                paths,
                renames,
            }) => {
                self.source
                    .apply_file_paths(catalog, &paths, &renames, &|| {
                        self.cancelled.load(Ordering::Acquire)
                    })
                    .await
            }
            SelectedFeedChange::Local(LocalLiveChange::Paths { paths, renames }) => {
                let (paths, renames) = LocalLiveChange::path_strings(paths, renames);
                self.source
                    .apply_file_paths(catalog, &paths, &renames, &|| {
                        self.cancelled.load(Ordering::Acquire)
                    })
                    .await
            }
            // A lost remote boundary checks what changed since the last sync; a file feed
            // walks everything.
            listing => {
                let refresh = match listing {
                    SelectedFeedChange::Remote(_) => Refresh::Check,
                    _ => Refresh::Changed,
                };
                if !list(refresh).await {
                    self.keep(listing);
                }
                Ok(())
            }
        }
    }
}

#[cfg(feature = "bindings")]
uniffi::custom_type!(usize, u64, {
    remote,
    lower: |value| value as u64,
    try_lift: |value| Ok(usize::try_from(value)?),
});

/// How far a source's read or download has come.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(feature = "bindings", derive(uniffi::Enum))]
pub enum SourceProgressStage {
    Connecting,
    Albums,
    Tracks,
    Artists,
    Genres,
    Playlists,
    Home,
    Artwork,
    Lyrics,
    Finalizing,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct SourceProgress {
    pub stage: SourceProgressStage,
    pub completed: usize,
    pub total: Option<usize>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct NativeImageRef {
    pub item_id: String,
    pub tag: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub blur_hash: Option<String>,
}

impl NativeImageRef {
    pub fn new(item_id: impl Into<String>, tag: impl Into<Option<String>>) -> Self {
        Self {
            item_id: item_id.into(),
            tag: tag.into(),
            blur_hash: None,
        }
    }
}

pub fn native_artwork_binding(
    source_id: &str,
    image: &NativeImageRef,
) -> Result<Vec<u8>, serde_json::Error> {
    serde_json::to_vec(&[library::ArtworkCandidate::new(
        library::ArtworkImage::Native {
            source_id: SourceId::new(source_id),
            item_id: image.item_id.clone(),
            tag: image.tag.clone(),
        },
        image.blur_hash.clone(),
    )])
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum LocalImageRef {
    File {
        source_id: SourceId,
        path: String,
        revision: String,
    },
    Embedded {
        source_id: SourceId,
        path: String,
        picture_index: u32,
        revision: String,
    },
}

impl LocalImageRef {
    pub fn artwork_binding(&self) -> Vec<u8> {
        let (source_id, path, revision, picture_index) = match self {
            Self::File {
                source_id,
                path,
                revision,
            } => (source_id, path, revision, None),
            Self::Embedded {
                source_id,
                path,
                revision,
                picture_index,
            } => (source_id, path, revision, Some(*picture_index)),
        };
        serde_json::to_vec(&[library::ArtworkCandidate::new(
            library::ArtworkImage::Local {
                source_id: source_id.clone(),
                path: path.clone(),
                revision: revision.clone(),
                picture_index,
            },
            None,
        )])
        .expect("local artwork binding")
    }

    pub fn source_id(&self) -> &SourceId {
        match self {
            Self::File { source_id, .. } | Self::Embedded { source_id, .. } => source_id,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum ImageSize {
    Original,
    Thumbnail(u32),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SourceImageRequest {
    Native {
        image_ref: NativeImageRef,
        size: ImageSize,
    },
    Local(LocalImageRef),
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

#[derive(Clone, Copy, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum SourceEntityKind {
    Track,
    Album,
    Artist,
}
impl SourceEntityKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Track => "track",
            Self::Album => "album",
            Self::Artist => "artist",
        }
    }
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

pub(crate) fn optional_collection_error(error: SourceError) -> SourceResult<()> {
    if matches!(error, SourceError::Cancelled | SourceError::Library(_)) {
        return Err(error);
    }
    tracing::warn!(%error, "optional source collection unavailable; retaining its cached observations");
    Ok(())
}

pub(crate) async fn acquire_home<F, Work>(
    changes: &mut library::Observation<'_>,
    cancelled: &(dyn Fn() -> bool + Send + Sync),
    mut read: F,
) -> SourceResult<()>
where
    F: FnMut(library::HomeSectionKind) -> Work,
    Work: std::future::Future<Output = SourceResult<Vec<library::HomeEntryInput>>>,
{
    for section in library::HomeSectionKind::SOURCE_SECTIONS {
        let Some(id) = section.source_section() else {
            continue;
        };
        match read(section).await {
            Ok(entries) => {
                if cancelled() {
                    return Err(SourceError::Cancelled);
                }
                changes
                    .replace_source_home_section(id, None, &(entries))
                    .await?;
            }
            Err(error) => optional_collection_error(error)?,
        }
    }
    Ok(())
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum RemoteItemUpdate {
    Metadata,
    UserData(library::NativeUserState),
    Remove,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum RemoteItemChange {
    Items(BTreeMap<String, RemoteItemUpdate>),
    BoundaryLost,
}

impl RemoteItemChange {
    pub(crate) fn items(upserts: Vec<String>, removals: Vec<String>) -> Self {
        let mut items: BTreeMap<_, _> = upserts
            .into_iter()
            .map(|id| (id, RemoteItemUpdate::Metadata))
            .collect();
        for id in removals {
            if matches!(
                items.insert(id, RemoteItemUpdate::Remove),
                Some(RemoteItemUpdate::Metadata)
            ) {
                return Self::BoundaryLost;
            }
        }
        if items.len() > LIVE_CHANGE_LIMIT {
            Self::BoundaryLost
        } else {
            Self::Items(items)
        }
    }

    fn merge(self, incoming: Self) -> Self {
        let (Self::Items(mut current), Self::Items(incoming)) = (self, incoming) else {
            return Self::BoundaryLost;
        };
        for (id, change) in incoming {
            if let Some(previous) = current.get_mut(&id) {
                match (previous, change) {
                    (
                        RemoteItemUpdate::UserData(previous),
                        RemoteItemUpdate::UserData(incoming),
                    ) => previous.merge(incoming),
                    (previous @ RemoteItemUpdate::UserData(_), RemoteItemUpdate::Metadata) => {
                        *previous = RemoteItemUpdate::Metadata
                    }
                    (
                        RemoteItemUpdate::Metadata,
                        RemoteItemUpdate::UserData(_) | RemoteItemUpdate::Metadata,
                    )
                    | (RemoteItemUpdate::Remove, RemoteItemUpdate::Remove) => {}
                    _ => return Self::BoundaryLost,
                }
            } else {
                current.insert(id, change);
            }
        }
        if current.len() > LIVE_CHANGE_LIMIT {
            Self::BoundaryLost
        } else {
            Self::Items(current)
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum LocalLiveChange {
    Paths {
        paths: Vec<PathBuf>,
        renames: Vec<(PathBuf, PathBuf)>,
    },
    Rescan,
}

impl LocalLiveChange {
    /// The changed paths and renames as the file pipeline names them.
    pub(crate) fn path_strings(
        paths: Vec<PathBuf>,
        renames: Vec<(PathBuf, PathBuf)>,
    ) -> (Vec<String>, Vec<(String, String)>) {
        let text = |path: PathBuf| path.to_string_lossy().into_owned();
        (
            paths.into_iter().map(text).collect(),
            renames
                .into_iter()
                .map(|(from, to)| (text(from), text(to)))
                .collect(),
        )
    }

    pub(crate) fn merge(self, incoming: Self) -> Self {
        match (self, incoming) {
            (Self::Rescan, _) | (_, Self::Rescan) => Self::Rescan,
            (
                Self::Paths {
                    paths: mut current,
                    mut renames,
                },
                Self::Paths {
                    paths: incoming,
                    renames: incoming_renames,
                },
            ) => {
                renames.extend(incoming_renames);
                renames.dedup();
                current.extend(incoming);
                current.sort();
                current.dedup();
                if current.len() > LIVE_CHANGE_LIMIT {
                    Self::Rescan
                } else {
                    Self::Paths {
                        paths: current,
                        renames,
                    }
                }
            }
        }
    }
}

pub struct ConnectedSource {
    configuration: SourceConfiguration,
    source: Source,
    credential: Option<String>,
}

impl ConnectedSource {
    pub(crate) fn plex(
        configuration: SourceConfiguration,
        source: crate::plex::PlexSource,
        credential: Option<String>,
    ) -> Self {
        Self {
            source: Source::new(
                configuration.source_id.clone(),
                Implementation::Plex(source),
            ),
            configuration,
            credential,
        }
    }
    pub(crate) fn files(
        configuration: SourceConfiguration,
        source: crate::file::remote::FileSource,
        credential: Option<String>,
    ) -> Self {
        Self {
            source: Source::new(
                configuration.source_id.clone(),
                Implementation::Files(source),
            ),
            configuration,
            credential,
        }
    }
    pub fn into_parts(self) -> (SourceConfiguration, Source, Option<String>) {
        (self.configuration, self.source, self.credential)
    }

    pub(crate) fn local(
        configuration: SourceConfiguration,
        source: crate::file::local::LocalSource,
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

    pub(crate) fn jellyfin_emby(
        configuration: SourceConfiguration,
        source: crate::jellyfin_emby::JellyfinEmbySource,
        credential: Option<String>,
    ) -> Self {
        Self {
            source: Source::new(
                configuration.source_id.clone(),
                Implementation::JellyfinEmby(source),
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

pub enum Implementation {
    Plex(crate::plex::PlexSource),
    Local(crate::file::local::LocalSource),
    Files(crate::file::remote::FileSource),
    JellyfinEmby(crate::jellyfin_emby::JellyfinEmbySource),
    OpenSubsonic(crate::subsonic::SubsonicSource),
    Peer(crate::peer::PeerSource),
}

pub struct Source {
    source_id: SourceId,
    retired: AtomicBool,
    /// The albums and artists, by URI, that a live search admitted while the Catalog held
    /// none of them: it holds only what the search found of them until they are fetched whole.
    partial: Mutex<HashSet<String>>,
    /// When the server last failed to connect, while that stands.
    unreachable: Mutex<Option<std::time::Instant>>,
    pub implementation: Implementation,
}

/// How long an unreachable server answers its reads at once before one tries it again.
const UNREACHABLE_RETRY: std::time::Duration = std::time::Duration::from_secs(30);

impl Source {
    pub fn file_connection(&self) -> SourceResult<&crate::FileSource> {
        match &self.implementation {
            Implementation::Files(source) => Ok(source),
            _ => Err(SourceError::InvalidRequest(
                "Choose a WebDAV or SMB connection",
            )),
        }
    }

    pub(crate) async fn catalog_operation<T, E>(
        &self,
        catalog: &Catalog,
        work: impl std::ops::AsyncFnOnce(&mut library::Observation<'_>) -> Result<T, E>,
    ) -> Result<T, E>
    where
        E: From<SourceError>,
    {
        if self.is_retired() {
            return Err(SourceError::Cancelled.into());
        }
        let mut observation = catalog
            .observe(self.source_id.as_str())
            .await
            .map_err(SourceError::from)?;
        work(&mut observation).await
    }

    fn file_source(&self, path: Option<&str>) -> Option<&crate::file::remote::FileSource> {
        match &self.implementation {
            Implementation::Files(source) => Some(source),
            Implementation::Local(local) => path.and_then(|path| local.document_source(path)),
            _ => None,
        }
    }

    pub async fn plex_companion_context(&self) -> SourceResult<crate::PlexCompanionContext> {
        Ok(self
            .plex_companion_source()?
            .companion_context(&self.source_id)
            .await)
    }
    fn plex_companion_source(&self) -> SourceResult<&crate::plex::PlexSource> {
        match &self.implementation {
            Implementation::Plex(source) => Ok(source),
            _ => Err(SourceError::InvalidRequest(
                "Native Plexamp playback requires one configured Plex server and profile",
            )),
        }
    }
    pub async fn plex_companion_players(&self) -> SourceResult<Vec<crate::PlexCompanionPlayer>> {
        self.plex_companion_source()?.companion_players().await
    }
    pub async fn plex_delegation_token(&self) -> SourceResult<String> {
        self.plex_companion_source()?.delegation_token().await
    }
    pub async fn plex_queue_window(
        &self,
        queue_id: u64,
        center: Option<u64>,
    ) -> SourceResult<crate::PlexQueueWindow> {
        self.plex_companion_source()?
            .queue_window(&self.source_id, queue_id, center)
            .await
    }
    pub async fn plex_queue_membership(
        &self,
        queue_id: u64,
        total: usize,
    ) -> SourceResult<crate::PlexQueueWindow> {
        self.plex_companion_source()?
            .queue_membership(&self.source_id, queue_id, total)
            .await
    }
    pub async fn plex_queue_adjacent(
        &self,
        queue_id: u64,
        center: u64,
        before: bool,
        center_offset: Option<usize>,
    ) -> SourceResult<crate::PlexQueueWindow> {
        self.plex_companion_source()?
            .queue_adjacent(&self.source_id, queue_id, center, before, center_offset)
            .await
    }
    pub async fn plex_queue_mutation(
        &self,
        mutation: crate::PlexQueueMutation,
    ) -> SourceResult<crate::PlexQueueWindow> {
        self.plex_companion_source()?
            .queue_mutation(&self.source_id, mutation)
            .await
    }
}

impl Source {
    pub fn plex_login(&self) -> Option<Arc<tokio::sync::Mutex<crate::PlexLogin>>> {
        match &self.implementation {
            Implementation::Plex(source) => Some(Arc::clone(&source.login)),
            _ => None,
        }
    }
    pub fn bind_plex_login(&mut self, login: Arc<tokio::sync::Mutex<crate::PlexLogin>>) {
        if let Implementation::Plex(source) = &mut self.implementation {
            source.login = login;
        }
    }
    fn track_object_id(&self, media_uri: &str) -> SourceResult<String> {
        library::source_entity_parts(media_uri)
            .filter(|(source_id, kind, _)| source_id == &self.source_id && kind == "track")
            .map(|(_, _, object_id)| object_id)
            .ok_or(SourceError::InvalidRequest(
                "media URI does not belong to this source",
            ))
    }

    pub(crate) fn new(source_id: SourceId, implementation: Implementation) -> Self {
        Self {
            source_id,
            retired: AtomicBool::new(false),
            partial: Mutex::default(),
            unreachable: Mutex::default(),
            implementation,
        }
    }

    /// Runs a request that has a cached answer or a report to drop. While the server is
    /// unreachable it fails at once, and one request every `UNREACHABLE_RETRY` tries it again.
    async fn reach<T>(&self, request: impl Future<Output = SourceResult<T>>) -> SourceResult<T> {
        {
            let mut unreachable = self
                .unreachable
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            if let Some(since) = *unreachable {
                if since.elapsed() < UNREACHABLE_RETRY {
                    return Err(SourceError::Network("The server is unreachable".into()));
                }
                *unreachable = Some(std::time::Instant::now());
            }
        }
        let result = request.await;
        let mut unreachable = self
            .unreachable
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        match &result {
            Ok(_) => *unreachable = None,
            Err(SourceError::Network(_)) => *unreachable = Some(std::time::Instant::now()),
            Err(_) => {}
        }
        result
    }

    /// The device joined or left a network: the server may answer again, at another address.
    pub async fn network_changed(&self) {
        *self
            .unreachable
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = None;
        self.select_address().await;
    }

    pub async fn connect(
        source_id: SourceId,
        input: SourceSetup,
        jellyfin_device_id: &str,
    ) -> SourceResult<ConnectedSource> {
        match input {
            SourceSetup::EmbyConnect {
                server,
                source_name,
                use_instant_mix,
            } => {
                crate::jellyfin_emby::connect::connect_server(
                    source_id,
                    server,
                    source_name,
                    use_instant_mix,
                    jellyfin_device_id.to_owned(),
                )
                .await
            }
            SourceSetup::JellyfinQuickConnect {
                login,
                source_name,
                use_instant_mix,
                local_server_url,
            } => {
                crate::jellyfin_emby::quick_connect::connect_quick(
                    source_id,
                    login,
                    source_name,
                    use_instant_mix,
                    local_server_url,
                )
                .await
            }
            SourceSetup::Plex(input) => crate::plex::connect(source_id, input).await,
            SourceSetup::WebDav {
                name,
                settings,
                credentials,
            } => {
                crate::file::remote::connect(source_id, "webdav", name, settings, credentials).await
            }
            SourceSetup::Smb {
                name,
                settings,
                credentials,
            } => crate::file::remote::connect(source_id, "smb", name, settings, credentials).await,
            SourceSetup::Local { roots } => crate::file::local::connect(source_id, roots),
            SourceSetup::JellyfinEmby {
                kind,
                credentials,
                use_instant_mix,
            } => {
                crate::jellyfin_emby::connect(
                    source_id,
                    kind,
                    credentials,
                    use_instant_mix,
                    jellyfin_device_id.to_owned(),
                )
                .await
            }
            SourceSetup::OpenSubsonic {
                kind,
                authentication,
                credentials,
            } => crate::subsonic::connect(source_id, kind, authentication, credentials).await,
        }
    }

    pub async fn edit(
        current: SourceConfiguration,
        current_credential: Option<String>,
        input: SourceSettingsChange,
        jellyfin_device_id: Option<String>,
    ) -> SourceResult<SourceEditResult> {
        match input {
            SourceSettingsChange::EmbyConnect {
                server,
                source_name,
                use_instant_mix,
                ..
            } => {
                let device_id = jellyfin_device_id.ok_or_else(|| {
                    SourceError::InvalidConfig("The app-wide device ID is missing".into())
                })?;
                crate::jellyfin_emby::connect::connect_server(
                    current.source_id,
                    server,
                    source_name,
                    use_instant_mix,
                    device_id,
                )
                .await
                .map(|source| SourceEditResult::Connected(Box::new(source)))
            }
            SourceSettingsChange::JellyfinQuickConnect {
                login,
                source_name,
                use_instant_mix,
                local_server_url,
                ..
            } => crate::jellyfin_emby::quick_connect::connect_quick(
                current.source_id,
                login,
                source_name,
                use_instant_mix,
                local_server_url,
            )
            .await
            .map(|source| SourceEditResult::Connected(Box::new(source))),
            SourceSettingsChange::Plex { settings, .. } => {
                crate::plex::edit(current, current_credential, settings).await
            }
            SourceSettingsChange::Files {
                name,
                settings,
                credentials,
                ..
            } => {
                crate::file::remote::edit(current, current_credential, name, settings, credentials)
                    .await
            }
            SourceSettingsChange::Local {
                roots,
                excluded_folders,
                ..
            } => crate::file::local::edit(current, roots, excluded_folders),
            SourceSettingsChange::LocalDocuments { roots, .. } => {
                let next = current.with_document_roots(roots)?;
                if next == current {
                    return Ok(SourceEditResult::Unchanged);
                }
                let source = crate::file::local::LocalSource::from_configuration(&next)?;
                Ok(SourceEditResult::Connected(Box::new(
                    ConnectedSource::local(next, source),
                )))
            }
            SourceSettingsChange::JellyfinEmby {
                connect_manually,
                credentials,
                use_instant_mix,
                ..
            } => {
                crate::jellyfin_emby::edit(
                    current,
                    current_credential,
                    connect_manually,
                    credentials,
                    use_instant_mix,
                    jellyfin_device_id,
                )
                .await
            }
            SourceSettingsChange::OpenSubsonic {
                authentication,
                credentials,
                ..
            } => {
                crate::subsonic::edit(current, current_credential, authentication, credentials)
                    .await
            }
            SourceSettingsChange::Peer { name, .. } => {
                let name = name.trim();
                if name.is_empty() {
                    return Err(SourceError::InvalidConfig("A source needs a name".into()));
                }
                Ok(if name == current.name {
                    SourceEditResult::Unchanged
                } else {
                    SourceEditResult::ConfigurationOnly(SourceConfiguration {
                        name: name.into(),
                        ..current
                    })
                })
            }
        }
    }

    /// Opens a configured source. A peer source reaches its device through `peer`, Rufin
    /// Connect's link.
    pub fn open(
        configuration: SourceConfiguration,
        credential: Option<String>,
        jellyfin_device_id: Option<String>,
        peer: Option<Arc<dyn crate::PeerLink>>,
    ) -> SourceResult<Self> {
        let implementation = match configuration.kind.as_str() {
            "plex" => Implementation::Plex(crate::plex::open(&configuration, credential)?),
            "smb" | "webdav" => Implementation::Files(crate::file::remote::FileSource::open(
                &configuration,
                credential,
            )?),
            crate::file::local::LOCAL_SOURCE_ID => Implementation::Local(
                crate::file::local::LocalSource::from_configuration(&configuration)?,
            ),
            "jellyfin" | "emby" => Implementation::JellyfinEmby(crate::jellyfin_emby::open(
                &configuration,
                credential,
                jellyfin_device_id,
            )?),
            "navidrome" | "subsonic" => {
                Implementation::OpenSubsonic(crate::subsonic::open(&configuration, credential)?)
            }
            "peer" => Implementation::Peer(crate::peer::PeerSource::open(&configuration, peer)?),
            kind => {
                return Err(SourceError::InvalidConfig(format!(
                    "unknown source kind {kind}"
                )));
            }
        };
        Ok(Self::new(configuration.source_id, implementation))
    }

    /// Chooses again between the server's local and public addresses, as when the network
    /// changed.
    pub async fn select_address(&self) {
        match &self.implementation {
            Implementation::JellyfinEmby(source) => source.select_address().await,
            Implementation::OpenSubsonic(source) => source.select_address().await,
            Implementation::Plex(_)
            | Implementation::Local(_)
            | Implementation::Files(_)
            | Implementation::Peer(_) => {}
        }
    }

    pub fn retire(&self) {
        self.retired.store(true, Ordering::Release);
    }
    pub fn is_retired(&self) -> bool {
        self.retired.load(Ordering::Acquire)
    }

    pub fn source_id(&self) -> &SourceId {
        &self.source_id
    }

    /// Check file metadata without opening a stream or transferring audio.
    pub async fn media_file_exists(&self, path: &str) -> SourceResult<bool> {
        if let Some(source) = self.file_source(Some(path)) {
            return source.media_file_exists(path).await;
        }
        match &self.implementation {
            Implementation::Files(source) => source.media_file_exists(path).await,
            _ => Err(SourceError::InvalidRequest(
                "The source is not a network file library",
            )),
        }
    }

    pub fn needs_periodic_refresh(&self) -> bool {
        match &self.implementation {
            Implementation::OpenSubsonic(_) | Implementation::Peer(_) => true,
            Implementation::Files(files) => !files.has_notifications(),
            _ => false,
        }
    }

    /// Lists what changed on the source, or all of it, into the Catalog. Returns whether the
    /// listing stored, moved or removed a playlist file.
    pub async fn acquire_catalog(
        &self,
        catalog: &Catalog,
        progress: &(dyn Fn(SourceProgress) + Send + Sync),
        cancelled: Arc<AtomicBool>,
        refresh: Refresh,
    ) -> SourceResult<bool> {
        if cancelled.load(Ordering::Acquire) || self.is_retired() {
            return Err(SourceError::Cancelled);
        }
        let check = refresh == Refresh::Check;
        let source_id = self.source_id.as_str();
        let freshness = self.freshness().await.ok().flatten();
        let cancelled_fn = || cancelled.load(Ordering::Acquire) || self.is_retired();
        if self.is_retired() || (&cancelled_fn)() {
            return Err(SourceError::Cancelled);
        }
        self.catalog_operation(catalog, async |changes| {
            let synced_at = catalog
                .source_key(&self.source_id)
                .and_then(|source| catalog.synced_at(source));
            // A check of a source with dated changes lists what changed since the last sync.
            let since = synced_at.filter(|_| check);
            if check {
                let accepted = if let Some(marker) = &freshness {
                    catalog
                        .accept_freshness(source_id, marker, &library::ReadCancellation::new())
                        .await?
                } else {
                    false
                };
                // A server without a scan marker lists everything once a day.
                let daily = synced_at.is_none_or(|at| unix_now() - at >= 24 * 60 * 60);
                let full = match (&self.implementation, &freshness) {
                    (Implementation::Plex(_) | Implementation::JellyfinEmby(_), _) => true,
                    (_, Some(_)) => !accepted,
                    (Implementation::OpenSubsonic(_), None) => daily,
                    (implementation, None) => matches!(implementation, Implementation::Files(_)),
                };
                if !full {
                    if let Implementation::OpenSubsonic(source) = &self.implementation {
                        source
                            .acquire_user_collections(changes, &|_| {}, &cancelled_fn)
                            .await?;
                    }
                    return Ok(false);
                }
            }
            changes.start_observation().await?;
            let mut started = unix_now();
            let report = |value| {
                if !check || !matches!(&self.implementation, Implementation::Files(_)) {
                    progress(value);
                }
            };
            let resync = refresh == Refresh::Resync;
            let complete = match &self.implementation {
                Implementation::Plex(source) => {
                    let date = source
                        .acquire_catalog(changes, since, &report, &cancelled_fn)
                        .await?;
                    started = date.unwrap_or(started);
                    true
                }
                Implementation::Files(source) => {
                    source
                        .walk(
                            changes,
                            crate::file::walk::WalkScope::All { resync },
                            &report,
                            &cancelled_fn,
                        )
                        .await?;
                    true
                }
                Implementation::Local(source) => {
                    source
                        .walk(
                            changes,
                            crate::file::walk::WalkScope::All { resync },
                            &report,
                            &cancelled_fn,
                        )
                        .await?;
                    true
                }
                Implementation::JellyfinEmby(source) => {
                    let date = source
                        .acquire_catalog(changes, since, &report, &cancelled_fn)
                        .await?;
                    started = date.unwrap_or(started);
                    true
                }
                Implementation::OpenSubsonic(source) => {
                    source
                        .acquire_catalog(changes, &report, &cancelled_fn)
                        .await?
                }
                // The sync time is the owner's clock, which its file times are compared with.
                Implementation::Peer(source) => {
                    started = source
                        .acquire_catalog(changes, since, &report, &cancelled_fn)
                        .await?;
                    true
                }
            };
            if cancelled_fn() || self.is_retired() {
                return Err(SourceError::Cancelled);
            }
            let files = matches!(
                self.implementation,
                Implementation::Local(_) | Implementation::Files(_)
            );
            changes
                .complete_source_observation(
                    &[],
                    freshness.as_ref().filter(|_| complete),
                    (!files && complete).then_some(started),
                )
                .await?;
            Ok(changes.playlist_files)
        })
        .await
    }

    async fn freshness(&self) -> SourceResult<Option<Freshness>> {
        match &self.implementation {
            Implementation::Plex(_) => Ok(None),
            Implementation::Local(_) | Implementation::JellyfinEmby(_) => Ok(None),
            Implementation::Files(source) => source.freshness().await,
            Implementation::OpenSubsonic(source) => source.freshness().await,
            Implementation::Peer(source) => source.freshness().await,
        }
    }

    /// The stream of a remote or document file, or of a server track.
    pub async fn stream(
        &self,
        playable: &library::Playable,
        request: StreamRequest,
    ) -> SourceResult<ResolvedStream> {
        self.reach(async {
            match &self.implementation {
                Implementation::Plex(source) => {
                    source
                        .resolve_stream(
                            &self.track_object_id(&playable.media_uri)?,
                            request.quality,
                            request.session_identifier.as_deref(),
                        )
                        .await
                }
                Implementation::Files(_) | Implementation::Local(_) => {
                    let file = playable.file.as_ref().ok_or(SourceError::NotFound)?;
                    self.file_source(Some(&file.path))
                        .ok_or(SourceError::NotFound)?
                        .stream(file, &playable.media_uri, playable.window)
                        .await
                }
                Implementation::JellyfinEmby(source) => {
                    source
                        .resolve_stream(
                            &self.track_object_id(&playable.media_uri)?,
                            request.quality,
                            request.session_identifier.as_deref(),
                        )
                        .await
                }
                Implementation::OpenSubsonic(source) => {
                    source
                        .resolve_stream(
                            &self.track_object_id(&playable.media_uri)?,
                            request.quality,
                        )
                        .await
                }
                Implementation::Peer(source) => source.stream(playable, request).await,
            }
        })
        .await
    }

    pub async fn resolve_download(
        &self,
        playable: &library::Playable,
        quality: DownloadQuality,
    ) -> SourceResult<ResolvedDownload> {
        let backing = library::cue_media_parts(&playable.media_uri)
            .map_or_else(|| playable.media_uri.clone(), |(_, uri, _, _)| uri);
        if library::file_media_path(&backing).is_some() {
            return Ok(ResolvedDownload::encode(
                ResolvedStream::new(backing),
                quality,
            ));
        }
        if let Some(uri) = playable.local_uri() {
            let stream = if uri.starts_with("content:") {
                crate::resolve_document_stream(&uri).await?
            } else {
                ResolvedStream::new(uri)
            };
            let stream = match playable.window {
                Some((start, end)) => stream.with_window(start, end),
                None => stream,
            };
            return Ok(ResolvedDownload::encode(stream, quality));
        }
        match &self.implementation {
            Implementation::Plex(source) => {
                source
                    .resolve_download(
                        &self.track_object_id(&playable.media_uri)?,
                        quality,
                        playable.channels,
                    )
                    .await
            }
            Implementation::Files(_) | Implementation::Local(_) => self
                .stream(
                    playable,
                    StreamRequest::original(playable.media_uri.clone()),
                )
                .await
                .map(|stream| ResolvedDownload::encode(stream, quality)),
            Implementation::Peer(source) => source.download(playable, quality).await,
            Implementation::JellyfinEmby(source) => {
                source
                    .resolve_download(
                        &self.track_object_id(&playable.media_uri)?,
                        quality,
                        playable.channels,
                    )
                    .await
            }
            Implementation::OpenSubsonic(source) => {
                source.resolve_download(&self.track_object_id(&playable.media_uri)?, quality)
            }
        }
    }

    pub async fn image(&self, request: SourceImageRequest) -> SourceResult<ImageBytes> {
        self.reach(async {
            match &self.implementation {
                Implementation::Plex(source) => match request {
                    SourceImageRequest::Native { image_ref, size } => {
                        source.image_bytes(&image_ref, size).await
                    }
                    SourceImageRequest::Local(_) => Err(SourceError::NotFound),
                },
                Implementation::Files(source) => source.image(request).await,
                Implementation::Local(source) => source.image(request).await,
                Implementation::JellyfinEmby(source) => match request {
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
                Implementation::Peer(source) => match request {
                    SourceImageRequest::Native { image_ref, size } => {
                        source.image(&image_ref, size).await
                    }
                    SourceImageRequest::Local(_) => Err(SourceError::NotFound),
                },
            }
        })
        .await
    }

    /// What the server finds for `query`, in music folder `music_folder` (its key and object
    /// id): the server narrows to it where its search takes a folder, else the Catalog does; the
    /// rows count their totals in it either way.
    pub async fn live_search(
        &self,
        catalog: &Catalog,
        query: &str,
        limit: usize,
        music_folder: Option<(library::FolderKey, &str)>,
    ) -> SourceResult<library::SearchResults> {
        self.reach(async {
            if matches!(
                self.implementation,
                Implementation::Local(_) | Implementation::Files(_) | Implementation::Peer(_)
            ) {
                return Err(SourceError::InvalidRequest("File Search is Catalog-owned"));
            }
            self.catalog_operation(catalog, async |changes| {
                // As a walk, feed writes newer than the request win; the server searches the folder.
                changes.start_observation().await?;
                let source_id = self.source_id.as_str();
                let folder_id = music_folder.map(|(_, id)| id);
                let batch = match &self.implementation {
                    Implementation::Plex(source) => {
                        source
                            .live_search(source_id, query, limit, folder_id)
                            .await?
                    }
                    Implementation::JellyfinEmby(source) => {
                        source
                            .live_search(source_id, query, limit, folder_id)
                            .await?
                    }
                    Implementation::OpenSubsonic(source) => {
                        source
                            .live_search(source_id, query, limit, folder_id)
                            .await?
                    }
                    _ => return Err(SourceError::InvalidRequest("File Search is Catalog-owned")),
                };
                let tracks = batch
                    .tracks
                    .iter()
                    .map(|v| v.object_id.clone())
                    .collect::<Vec<_>>();
                let albums = batch
                    .albums
                    .iter()
                    .map(|v| v.object_id.clone())
                    .collect::<Vec<_>>();
                let artists = batch
                    .artists
                    .iter()
                    .map(|v| v.object_id.clone())
                    .collect::<Vec<_>>();
                if self.is_retired() {
                    return Err(SourceError::Cancelled);
                }
                let partial = self.unheld_collections(catalog, &batch);
                changes.apply_catalog(&(batch), true).await?;
                self.partial
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner())
                    .extend(partial);
                Ok(catalog
                    .search_rows_by_objects(
                        changes.source,
                        music_folder.map(|(key, _)| key),
                        false,
                        &tracks,
                        &albums,
                        &artists,
                        &library::ReadCancellation::new(),
                    )
                    .await?)
            })
            .await
        })
        .await
    }

    pub async fn set_favorite(
        &self,
        kind: SourceEntityKind,
        object_id: &str,
        favorite: bool,
    ) -> SourceResult<()> {
        match &self.implementation {
            Implementation::Plex(_) => Ok(()),
            Implementation::Local(_) | Implementation::Files(_) | Implementation::Peer(_) => Ok(()),
            Implementation::JellyfinEmby(source) => source.set_favorite(object_id, favorite).await,
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
            Implementation::Plex(source) => source.set_rating(object_id, rating).await,
            Implementation::Local(_) | Implementation::Files(_) | Implementation::Peer(_) => Ok(()),
            Implementation::JellyfinEmby(source) => source.set_rating(object_id, rating).await,
            Implementation::OpenSubsonic(source) => source.set_rating(object_id, rating).await,
        }
    }

    pub fn supports_playlist_artwork(&self) -> bool {
        match &self.implementation {
            Implementation::JellyfinEmby(_) | Implementation::Plex(_) => true,
            Implementation::OpenSubsonic(provider) => provider.supports_playlist_artwork(),
            Implementation::Local(_) | Implementation::Files(_) | Implementation::Peer(_) => false,
        }
    }

    pub async fn cached_lyrics(
        &self,
        catalog: &Catalog,
        media_uri: &str,
    ) -> SourceResult<Option<lyrics::LyricsBundle>> {
        if let Some(bundle) = Self::stored_lyrics(catalog, media_uri).await {
            return Ok(Some(bundle));
        }
        let native = match self.read_lyrics(catalog, media_uri).await {
            Err(SourceError::NotFound) => None,
            result => result?,
        };
        let mut bundle = native;
        if let Err(error) = cache_native_lyrics(catalog, media_uri, &mut bundle).await {
            tracing::warn!(%error, "could not save source lyrics cache");
        }
        if bundle.is_none() {
            // Bulk downloads skip known missing lyrics; opening the lyrics still retries.
            catalog
                .write_lyrics_cache(
                    media_uri,
                    "source",
                    "original",
                    "",
                    "",
                    "",
                    unix_now(),
                    false,
                )
                .await?;
        }
        Ok(bundle)
    }

    /// A track's cached source lyrics; a row that cannot show goes.
    pub async fn stored_lyrics(catalog: &Catalog, media_uri: &str) -> Option<lyrics::LyricsBundle> {
        match catalog
            .lyrics_cache_for_role(
                media_uri,
                "original",
                "",
                "",
                "source",
                &library::ReadCancellation::new(),
            )
            .await
        {
            Ok(Some(cached)) => {
                if let Some(bundle) = lyrics::LyricsBundle::from_cache_payload(&cached.lyrics)
                    .and_then(lyrics::lyrics_with_displayable_content)
                    .filter(|bundle| {
                        bundle.origin == lyrics::LyricsOrigin::Native
                            || bundle.origin == lyrics::LyricsOrigin::Local
                                && library::cue_media_parts(media_uri).is_none()
                    })
                {
                    return Some(bundle);
                }
                let _ = catalog
                    .remove_lyrics_cache(
                        media_uri,
                        "source",
                        &cached.role,
                        &cached.language,
                        &cached.script,
                    )
                    .await;
            }
            Ok(None) => {}
            Err(error) => tracing::warn!(%error, "could not read source lyrics cache"),
        }
        None
    }

    async fn read_lyrics(
        &self,
        catalog: &Catalog,
        media_uri: &str,
    ) -> SourceResult<Option<lyrics::LyricsBundle>> {
        let native_text = |text: String| {
            lyrics::lyrics_from_edited_text(&text).map(|mut bundle| {
                bundle.origin = lyrics::LyricsOrigin::Native;
                bundle
            })
        };
        match &self.implementation {
            Implementation::Plex(source) => source.lyrics(&self.track_object_id(media_uri)?).await,
            Implementation::Files(source) => {
                let playable = catalog
                    .playable_by_uri(media_uri)
                    .await?
                    .ok_or(SourceError::NotFound)?;
                source
                    .lyrics(catalog, &playable)
                    .await
                    .map(|text| text.and_then(native_text))
            }
            Implementation::Local(_) => {
                let playable = catalog.playable_by_uri(media_uri).await?;
                if let Some((source, playable)) = playable.as_ref().and_then(|playable| {
                    let file = playable.file.as_ref()?;
                    Some((self.file_source(Some(&file.path))?, playable))
                }) {
                    return source
                        .lyrics(catalog, playable)
                        .await
                        .map(|text| text.and_then(native_text));
                }
                read_local_lyrics(
                    catalog,
                    media_uri,
                    None,
                    None,
                    &lyrics::UserPreferences::default(),
                )
                .await
            }
            Implementation::JellyfinEmby(source) => {
                source.lyrics(&self.track_object_id(media_uri)?).await
            }
            Implementation::OpenSubsonic(source) => {
                source.lyrics(&self.track_object_id(media_uri)?).await
            }
            // The owner's source lyrics arrive with its observations.
            Implementation::Peer(_) => Err(SourceError::NotFound),
        }
    }

    pub async fn browse_folder(
        &self,
        catalog: &Catalog,
        folder_object_id: Option<&str>,
        music_folder_object_id: Option<&str>,
    ) -> SourceResult<LiveFolderPage> {
        self.reach(async {
            match &self.implementation {
                Implementation::Plex(source) => {
                    source
                        .browse_folder(folder_object_id, music_folder_object_id)
                        .await
                }
                Implementation::Local(_) | Implementation::Files(_) => {
                    catalog.loaded().await;
                    let source = catalog
                        .source_key(&self.source_id)
                        .ok_or(SourceError::NotFound)?;
                    let roots = match &self.implementation {
                        Implementation::Local(local) => local.configured_file_roots()?,
                        Implementation::Files(files) => vec![files.location("")?],
                        _ => unreachable!(),
                    };
                    let (folders, tracks) = catalog
                        .local_folder_contents(
                            source,
                            folder_object_id.or(music_folder_object_id),
                            &roots,
                        )
                        .await?;
                    Ok(LiveFolderPage {
                        folders: folders
                            .into_iter()
                            .map(|(object_id, name)| {
                                let name = match &self.implementation {
                                    Implementation::Local(local) => local
                                        .document_root_name(&object_id)
                                        .map(str::to_owned)
                                        .unwrap_or(name),
                                    _ => name,
                                };
                                LiveFolder { object_id, name }
                            })
                            .collect(),
                        tracks,
                    })
                }
                Implementation::JellyfinEmby(source) => {
                    source
                        .browse_folder(folder_object_id, music_folder_object_id)
                        .await
                }
                Implementation::OpenSubsonic(source) => {
                    source
                        .browse_folder(folder_object_id, music_folder_object_id)
                        .await
                }
                // A paired device's tracks carry no folders; its cached scope lists them.
                Implementation::Peer(_) => Err(SourceError::NotFound),
            }
        })
        .await
    }

    pub async fn generated_track_object_ids(
        &self,
        seed: &SourceRadioSeed,
        limit: usize,
    ) -> SourceResult<Vec<String>> {
        match &self.implementation {
            Implementation::Plex(source) => source.generated_track_object_ids(seed, limit).await,
            Implementation::Local(_) | Implementation::Files(_) | Implementation::Peer(_) => {
                Ok(Vec::new())
            }
            Implementation::JellyfinEmby(source) => {
                source.generated_track_object_ids(seed, limit).await
            }
            Implementation::OpenSubsonic(source) => {
                source.generated_track_object_ids(seed, limit).await
            }
        }
    }

    pub async fn import_playlist_files(
        &self,
        catalog: &Catalog,
        playlist: library::PlaylistKey,
    ) -> SourceResult<()> {
        let Implementation::Local(source) = &self.implementation else {
            return Ok(());
        };
        self.catalog_operation(catalog, async |changes| {
            source
                .import_playlist_files(changes, playlist, &|| self.is_retired())
                .await
        })
        .await
    }

    pub async fn import_local_paths(
        &self,
        catalog: &Catalog,
        paths: Vec<std::path::PathBuf>,
        name: &str,
        progress: &(dyn Fn(SourceProgress) + Send + Sync),
    ) -> SourceResult<library::PlaylistKey> {
        let Implementation::Local(local) = &self.implementation else {
            return Err(SourceError::InvalidRequest("Only Local imports files"));
        };
        self.catalog_operation(catalog, async |changes| {
            crate::file::local::scan::import_paths(changes, local, paths, name, progress, &|| {
                self.is_retired()
            })
            .await
        })
        .await
    }

    pub async fn import_playlist_file(
        &self,
        catalog: &Catalog,
        path: &str,
        target: Option<library::PlaylistKey>,
    ) -> SourceResult<library::PlaylistImportReport> {
        if let Implementation::Local(local) = &self.implementation
            && let Some((source, path)) = local.document_playlist(path)
        {
            return source.import_playlist_file(catalog, &path, target).await;
        }
        match &self.implementation {
            Implementation::Files(source) => {
                source.import_playlist_file(catalog, path, target).await
            }
            Implementation::Local(_) => {
                let path = self
                    .local_playlist_path(path)
                    .ok_or(SourceError::NotFound)?;
                crate::import_local_playlist_file(catalog, &path, target, |_| None).await
            }
            _ => Err(SourceError::InvalidRequest(
                "This source does not store playlist files",
            )),
        }
    }

    pub async fn save_playlist_file(
        &self,
        catalog: &Catalog,
        path: &str,
        file: tempfile::TempPath,
        path_mode: library::PlaylistPathMode,
        expected_revision: Option<&str>,
    ) -> SourceResult<()> {
        if let Implementation::Local(local) = &self.implementation
            && let Some((source, path)) = local.document_playlist(path)
        {
            return source
                .save_playlist_file(catalog, &path, file, path_mode, expected_revision)
                .await;
        }
        match &self.implementation {
            Implementation::Files(source) => {
                source
                    .save_playlist_file(catalog, path, file, path_mode, expected_revision)
                    .await
            }
            _ => Err(SourceError::InvalidRequest(
                "This source does not store playlist files",
            )),
        }
    }

    pub fn local_playlist_path(&self, path: &str) -> Option<std::path::PathBuf> {
        let Implementation::Local(source) = &self.implementation else {
            return None;
        };
        let (index, relative) = path
            .strip_prefix('@')
            .and_then(|value| value.split_once('/'))
            .and_then(|(index, relative)| {
                index.parse::<usize>().ok().map(|index| (index, relative))
            })
            .unwrap_or((0, path));
        source.roots().get(index).map(|root| root.join(relative))
    }

    pub async fn playlist_file_revision(&self, path: &str) -> SourceResult<String> {
        if let Implementation::Local(local) = &self.implementation
            && let Some((source, path)) = local.document_playlist(path)
        {
            return source.playlist_file_revision(&path).await;
        }
        match &self.implementation {
            Implementation::Local(_) => crate::playlist_file_revision(
                &self
                    .local_playlist_path(path)
                    .ok_or(SourceError::NotFound)?,
            )
            .map_err(|error| SourceError::Other(error.to_string())),
            Implementation::Files(source) => source.playlist_file_revision(path).await,
            _ => Err(SourceError::InvalidRequest(
                "This source does not store playlist files",
            )),
        }
    }

    pub async fn rename_playlist_file(&self, from: &str, to: &str) -> SourceResult<()> {
        if let Implementation::Local(local) = &self.implementation
            && let Some((source, from)) = local.document_playlist(from)
        {
            let (_, to) = local.document_playlist(to).ok_or(SourceError::NotFound)?;
            return source.rename_playlist_file(&from, &to).await;
        }
        match &self.implementation {
            Implementation::Files(source) => source.rename_playlist_file(from, to).await,
            _ => Err(SourceError::InvalidRequest(
                "Use native file access for this source",
            )),
        }
    }

    pub async fn delete_playlist_file(&self, path: &str) -> SourceResult<()> {
        if let Implementation::Local(local) = &self.implementation
            && let Some((source, path)) = local.document_playlist(path)
        {
            return source.remove_file(&path).await;
        }
        match &self.implementation {
            Implementation::Local(_) => std::fs::remove_file(
                self.local_playlist_path(path)
                    .ok_or(SourceError::NotFound)?,
            )
            .map_err(|error| SourceError::Other(error.to_string())),
            Implementation::Files(source) => source.remove_file(path).await,
            _ => Err(SourceError::InvalidRequest(
                "This source does not store playlist files",
            )),
        }
    }

    pub async fn apply_items(
        &self,
        catalog: &Catalog,
        upserts: Vec<String>,
        removals: Vec<String>,
    ) -> SourceResult<()> {
        self.catalog_operation(catalog, async |changes| {
            self.acquire_items(changes, upserts, removals).await?;
            self.acquire_item_relations(changes).await
        })
        .await
    }

    /// Reads these playlists again after an edit, without the relations a feed update reads.
    pub async fn apply_playlists(
        &self,
        catalog: &Catalog,
        upserts: Vec<String>,
        removals: Vec<String>,
    ) -> SourceResult<()> {
        self.catalog_operation(catalog, async |changes| {
            self.acquire_items(changes, upserts, removals).await
        })
        .await
    }

    pub(crate) async fn apply_changes(
        &self,
        catalog: &Catalog,
        items: BTreeMap<String, RemoteItemUpdate>,
    ) -> SourceResult<()> {
        self.catalog_operation(catalog, async |changes| {
            let (mut upserts, mut removals) = (Vec::new(), Vec::new());
            let mut user_data =
                BTreeMap::<&'static str, Vec<(String, library::NativeUserState)>>::new();
            for (id, change) in items {
                match change {
                    RemoteItemUpdate::Remove => removals.push(id),
                    RemoteItemUpdate::Metadata => upserts.push(id),
                    // User data applies to what the Catalog holds; new items arrive as
                    // library changes, and other ids (videos) are not music.
                    RemoteItemUpdate::UserData(facts) => {
                        if let Implementation::JellyfinEmby(provider) = &self.implementation {
                            for kind in ["track", "album", "artist"] {
                                user_data
                                    .entry(kind)
                                    .or_default()
                                    .push((provider.kind.object_id(kind, &id), facts.clone()));
                            }
                        }
                    }
                }
            }
            let metadata = async {
                if !upserts.is_empty() || !removals.is_empty() {
                    self.acquire_items(changes, upserts, removals).await?;
                    self.acquire_item_relations(changes).await?;
                }
                Ok(())
            }
            .await;
            for (kind, rows) in user_data {
                changes.write_native_user_data(kind, &rows).await?;
            }
            metadata
        })
        .await
    }

    pub(crate) async fn apply_file_paths(
        &self,
        catalog: &Catalog,
        paths: &[String],
        renames: &[(String, String)],
        cancelled: &(dyn Fn() -> bool + Send + Sync),
    ) -> SourceResult<()> {
        if self.is_retired() || (cancelled)() {
            return Err(SourceError::Cancelled);
        }
        self.catalog_operation(catalog, async |changes| {
            self.acquire_paths(changes, paths, renames, cancelled).await
        })
        .await
    }

    /// A live change: the changed paths are walked in a live observation.
    pub(crate) async fn acquire_paths(
        &self,
        changes: &mut library::Observation<'_>,
        paths: &[String],
        renames: &[(String, String)],
        cancelled: &(dyn Fn() -> bool + Send + Sync),
    ) -> SourceResult<()> {
        let cancelled = || self.is_retired() || cancelled();
        if cancelled() {
            return Err(SourceError::Cancelled);
        }
        let scope = crate::file::walk::WalkScope::Paths { paths, renames };
        match &self.implementation {
            Implementation::Local(local) => {
                local.walk(changes, scope, &|_| {}, &cancelled).await?;
            }
            Implementation::Files(files) => {
                files.walk(changes, scope, &|_| {}, &cancelled).await?;
            }
            _ => {
                return Err(SourceError::InvalidRequest(
                    "Source does not have file paths",
                ));
            }
        }
        if cancelled() {
            return Err(SourceError::Cancelled);
        }
        Ok(())
    }

    pub(crate) async fn acquire_items(
        &self,
        changes: &mut library::Observation<'_>,
        upserts: Vec<String>,
        removals: Vec<String>,
    ) -> SourceResult<()> {
        match &self.implementation {
            Implementation::JellyfinEmby(source) => {
                source
                    .acquire_items(changes, upserts, removals, &|| self.is_retired())
                    .await
            }
            Implementation::Plex(source) => {
                source
                    .acquire_items(changes, upserts, removals, &|| self.is_retired())
                    .await
            }
            Implementation::OpenSubsonic(source) => {
                let result = source
                    .acquire_items(changes, upserts, &|| self.is_retired())
                    .await;
                for object in removals {
                    if let Some((kind, id)) = object.split_once(':') {
                        let kind = match kind {
                            "song" => "track",
                            "album" => "album",
                            "artist" => "artist",
                            "playlist" => "playlist",
                            _ => continue,
                        };
                        changes
                            .remove_source_items(kind, &[source.id(kind, id)])
                            .await?;
                    }
                }
                result
            }
            _ => Err(SourceError::InvalidRequest(
                "Source has no remote item feed",
            )),
        }
    }

    fn item_request_id(&self, kind: &str, object: &str) -> String {
        let raw = raw_item_id(object);
        match &self.implementation {
            Implementation::OpenSubsonic(_) => {
                format!("{}:{raw}", if kind == "track" { "song" } else { kind })
            }
            _ => raw.to_string(),
        }
    }

    pub(crate) async fn acquire_item_relations(
        &self,
        changes: &mut library::Observation<'_>,
    ) -> SourceResult<()> {
        let catalog = changes.catalog;
        let source_id = self.source_id.as_str();
        let section = library::HomeSectionKind::NewlyAdded;
        let id = section.source_section().unwrap_or_default();
        match self.home_section(section).await {
            Ok(entries) => {
                let mut missing = Vec::new();
                for entry in &entries {
                    let kind = match entry.kind {
                        library::HomeEntryKind::Album => "album",
                        library::HomeEntryKind::Track => "track",
                        _ => continue,
                    };
                    if !catalog
                        .source_contains_entity(source_id, kind, &entry.entity_object_id, true)
                        .await?
                    {
                        missing.push(self.item_request_id(kind, &entry.entity_object_id));
                    }
                }
                if !missing.is_empty() {
                    if let Err(error) = self.acquire_items(changes, missing, Vec::new()).await {
                        optional_collection_error(error)?;
                    }
                }
                if self.is_retired() {
                    return Err(SourceError::Cancelled);
                }
                changes
                    .replace_source_home_section(id, None, &(entries))
                    .await?;
            }
            Err(error) => optional_collection_error(error)?,
        }
        Ok(())
    }

    /// The albums and artists `batch` names that the Catalog holds none of, by URI.
    fn unheld_collections(&self, catalog: &Catalog, batch: &library::CatalogBatch) -> Vec<String> {
        let credits = |credits: &[library::ArtistCredit]| {
            credits
                .iter()
                .map(|credit| credit.object_id.clone())
                .collect::<Vec<_>>()
        };
        let albums = batch
            .albums
            .iter()
            .map(|album| album.object_id.clone())
            .chain(
                batch
                    .tracks
                    .iter()
                    .filter_map(|track| track.album_object_id.clone()),
            );
        let artists = batch
            .artists
            .iter()
            .map(|artist| artist.object_id.clone())
            .chain(
                batch
                    .albums
                    .iter()
                    .flat_map(|album| credits(&album.metadata.album_artists)),
            )
            .chain(batch.tracks.iter().flat_map(|track| {
                let mut ids = credits(&track.metadata.artists);
                ids.extend(credits(&track.metadata.album_artists));
                ids
            }));
        let uris = albums
            .map(|id| ("album", id))
            .chain(artists.map(|id| ("artist", id)))
            .filter(|(_, id)| !id.is_empty())
            .map(|(kind, id)| library::source_entity_uri(&self.source_id, kind, &id))
            .collect::<Vec<_>>();
        let held = catalog.items(&uris.iter().map(String::as_str).collect::<Vec<_>>());
        let mut unheld = (uris.into_iter().zip(held))
            .filter_map(|(uri, item)| item.is_none().then_some(uri))
            .collect::<Vec<_>>();
        unheld.sort_unstable();
        unheld.dedup();
        unheld
    }

    /// Fetches the album or artist `media_uri` whole when a live search admitted only part of
    /// it; anything else the Catalog already holds whole.
    pub async fn complete_collection(
        &self,
        catalog: &Catalog,
        media_uri: &str,
    ) -> SourceResult<()> {
        self.reach(async {
            if !self
                .partial
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .contains(media_uri)
            {
                return Ok(());
            }
            let Some((_, kind, object_id)) = library::source_entity_parts(media_uri) else {
                return Err(SourceError::InvalidRequest("collection URI is unavailable"));
            };
            let collection = match kind.as_str() {
                "album" => SourceCollection::Album(object_id),
                "artist" => SourceCollection::Artist(object_id),
                _ => return Err(SourceError::InvalidRequest("URI is not a collection")),
            };
            self.catalog_operation(catalog, async |changes| {
                match &self.implementation {
                    Implementation::Plex(source) => {
                        source
                            .acquire_collection(changes, &collection, None, false, &|| {
                                self.is_retired()
                            })
                            .await?
                    }
                    Implementation::JellyfinEmby(source) => {
                        source
                            .acquire_collection(changes, &collection, None, false, &|| {
                                self.is_retired()
                            })
                            .await?
                    }
                    Implementation::OpenSubsonic(source) => {
                        source
                            .acquire_collection(changes, &collection, false, &|| self.is_retired())
                            .await?
                    }
                    Implementation::Local(_)
                    | Implementation::Files(_)
                    | Implementation::Peer(_) => {}
                }
                Ok::<_, SourceError>(())
            })
            .await?;
            self.partial
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .remove(media_uri);
            Ok(())
        })
        .await
    }

    pub async fn refresh_home_section(
        &self,
        catalog: &Catalog,
        section: library::HomeSectionKind,
    ) -> SourceResult<()> {
        let Some(id) = section.source_section() else {
            return Ok(());
        };
        self.catalog_operation(catalog, async |observation| {
            let entries = self.home_section(section).await?;
            if self.is_retired() {
                return Err(SourceError::Cancelled);
            }
            observation
                .replace_source_home_section(id, None, &entries)
                .await?;
            Ok(())
        })
        .await
    }

    pub async fn home_section(
        &self,
        section: library::HomeSectionKind,
    ) -> SourceResult<Vec<library::HomeEntryInput>> {
        match &self.implementation {
            Implementation::Plex(source) => source.home_section(section).await,
            Implementation::Local(_) | Implementation::Files(_) | Implementation::Peer(_) => {
                Ok(Vec::new())
            }
            Implementation::JellyfinEmby(source) => source.home_section(section).await,
            Implementation::OpenSubsonic(source) => source.home_section(section).await,
        }
    }

    pub fn start_selected_feed(
        self: &Arc<Self>,
        runtime: &tokio::runtime::Handle,
        mut initial_scan_pending: bool,
    ) -> Arc<SelectedFeed> {
        let (wake, receiver) = async_channel::bounded(1);
        let feed = Arc::new(SelectedFeed {
            source: Arc::clone(self),
            pending: Mutex::new(None),
            wake,
            cancelled: Arc::new(AtomicBool::new(false)),
            remote_task: Mutex::new(None),
            receiver,
        });
        match &self.implementation {
            Implementation::Local(local) if !local.roots().is_empty() => {
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
            Implementation::Plex(_) => {
                let producer = Arc::clone(&feed);
                let task = runtime.spawn(async move {
                    // A connect after the first, or the first when no listing runs, catches up.
                    let ready = || {
                        std::mem::take(&mut initial_scan_pending)
                            || producer
                                .submit(SelectedFeedChange::Remote(RemoteItemChange::BoundaryLost))
                    };
                    let changed = |change| producer.submit(SelectedFeedChange::Remote(change));
                    if let Implementation::Plex(source) = &producer.source.implementation {
                        source.listen_library_changes(ready, changed).await;
                    }
                });
                *feed.remote_task.lock().unwrap_or_else(|p| p.into_inner()) =
                    Some(task.abort_handle());
            }
            Implementation::JellyfinEmby(_) => {
                let producer = Arc::clone(&feed);
                let task = runtime.spawn(async move {
                    let ready_feed = Arc::clone(&producer);
                    let mut ready = move || {
                        if std::mem::take(&mut initial_scan_pending) {
                            return !ready_feed.cancelled.load(Ordering::Acquire);
                        }
                        ready_feed
                            .submit(SelectedFeedChange::Remote(RemoteItemChange::BoundaryLost))
                    };
                    let change_feed = Arc::clone(&producer);
                    let mut changed =
                        move |change| change_feed.submit(SelectedFeedChange::Remote(change));
                    let result = match &producer.source.implementation {
                        Implementation::JellyfinEmby(provider) => {
                            provider
                                .listen_library_changes(&mut ready, &mut changed)
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
                    producer.submit(SelectedFeedChange::Remote(RemoteItemChange::BoundaryLost));
                });
                *feed
                    .remote_task
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(task.abort_handle());
            }
            Implementation::Files(files) if files.has_notifications() => {
                let producer = Arc::clone(&feed);
                let task = runtime.spawn(async move {
                    if let Implementation::Files(files) = &producer.source.implementation {
                        files
                            .watch(|change| producer.submit(SelectedFeedChange::Files(change)))
                            .await;
                    }
                });
                *feed.remote_task.lock().unwrap_or_else(|p| p.into_inner()) =
                    Some(task.abort_handle());
            }
            Implementation::OpenSubsonic(source) if source.has_navidrome_library() => {
                let producer = Arc::clone(&feed);
                let task = runtime.spawn(async move {
                    if let Implementation::OpenSubsonic(source) = &producer.source.implementation {
                        if let Err(error) = source
                            .listen_navidrome_changes(|change| {
                                producer.submit(SelectedFeedChange::Remote(change))
                            })
                            .await
                        {
                            tracing::warn!(%error, "Navidrome change feed stopped");
                        }
                    }
                });
                *feed.remote_task.lock().unwrap_or_else(|p| p.into_inner()) =
                    Some(task.abort_handle());
            }
            Implementation::Local(_)
            | Implementation::Files(_)
            | Implementation::OpenSubsonic(_)
            | Implementation::Peer(_) => {}
        }
        feed
    }

    pub async fn report_playback(&self, report: &SourceReportFact) -> SourceResult<()> {
        self.reach(async {
            let object_id = self.track_object_id(&report.media_uri)?;
            match &self.implementation {
                Implementation::Plex(source) => source.report_playback(&object_id, report).await,
                Implementation::Local(_) | Implementation::Files(_) | Implementation::Peer(_) => {
                    Ok(())
                }
                Implementation::JellyfinEmby(source) => {
                    source.report_playback(&object_id, report).await
                }
                Implementation::OpenSubsonic(source) => {
                    source.report_playback(&object_id, report).await
                }
            }
        })
        .await
    }

    /// Checks a mapping against a representative track: its root, and the server prefix that
    /// maps the track to a file. Tracks resolve through the saved mapping when they are read.
    pub async fn check_local_mapping(
        &self,
        catalog: &Catalog,
        source: library::SourceKey,
        root: &std::path::Path,
        server_prefix: Option<&str>,
        local_prefix: Option<&str>,
        sample_source_path: Option<&str>,
    ) -> SourceResult<(PathBuf, Option<String>)> {
        let sample = catalog
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
        let root = root.to_path_buf();
        let server_prefix = server_prefix.map(str::to_owned);
        let local_prefix = local_prefix.map(str::to_owned);
        tokio::task::spawn_blocking(move || {
            let root = mapping_root(&root)?;
            let server_prefix = match_local_access_sample(
                &root,
                server_prefix.as_deref(),
                local_prefix.as_deref(),
                &sample.source_path,
            )
            .ok_or(SourceError::InvalidRequest(
                "The representative Track does not map to a local file",
            ))?;
            Ok((root, server_prefix))
        })
        .await
        .map_err(|error| SourceError::Other(error.to_string()))?
    }
}

fn mapping_root(root: &std::path::Path) -> SourceResult<PathBuf> {
    if root.to_string_lossy().starts_with("content:") {
        let entry = crate::file::documents::stat_document(&root.to_string_lossy())?;
        return if entry.directory {
            Ok(PathBuf::from(entry.uri))
        } else {
            Err(SourceError::NotFound)
        };
    }
    std::fs::canonicalize(root).map_err(|error| SourceError::Other(error.to_string()))
}

fn mapped_document_path(
    server_prefix: Option<&str>,
    local_prefix: Option<&str>,
    path: &str,
) -> Option<String> {
    let relative = library::project_local_access_path(
        std::path::Path::new(""),
        server_prefix,
        local_prefix,
        path,
    )?;
    if relative.components().any(|part| {
        !matches!(
            part,
            std::path::Component::Normal(_) | std::path::Component::CurDir
        )
    }) {
        return None;
    }
    Some(relative.to_string_lossy().into_owned())
}

fn mapping_path_exists(
    root: &std::path::Path,
    server: Option<&str>,
    local: Option<&str>,
    path: &str,
) -> bool {
    if root.to_string_lossy().starts_with("content:") {
        return mapped_document_path(server, local, path)
            .and_then(|path| {
                crate::file::documents::find_document(
                    &root.to_string_lossy(),
                    &path.split('/').map(str::to_owned).collect::<Vec<_>>(),
                )
                .ok()
                .flatten()
            })
            .is_some_and(|entry| !entry.directory);
    }
    mapped_local_path(root, server, local, path).is_some()
}

fn mapped_local_path(
    root: &std::path::Path,
    server_prefix: Option<&str>,
    local_prefix: Option<&str>,
    source_path: &str,
) -> Option<PathBuf> {
    let candidate =
        library::project_local_access_path(root, server_prefix, local_prefix, source_path)?;
    mapped_file(root, candidate)
}

fn match_local_access_sample(
    root: &std::path::Path,
    server_prefix: Option<&str>,
    local_prefix: Option<&str>,
    source_path: &str,
) -> Option<Option<String>> {
    let root = mapping_root(root).ok()?;
    let server_prefix = server_prefix
        .map(str::trim)
        .filter(|prefix| !prefix.is_empty());
    if mapping_path_exists(&root, server_prefix, local_prefix, source_path) {
        return Some(server_prefix.map(str::to_string));
    }
    if server_prefix.is_some() {
        return None;
    }
    let inferred = infer_server_prefix(&root, local_prefix, source_path)?;
    mapping_path_exists(&root, Some(&inferred), local_prefix, source_path).then_some(Some(inferred))
}

fn infer_server_prefix(
    root: &std::path::Path,
    local_prefix: Option<&str>,
    source_path: &str,
) -> Option<String> {
    let starts = std::iter::once(0).chain(
        source_path
            .char_indices()
            .filter(|(_, character)| *character == '/' || *character == '\\')
            .map(|(index, character)| index + character.len_utf8()),
    );
    for start in starts {
        let suffix = source_path.get(start..)?.trim_start_matches(['/', '\\']);
        if suffix.is_empty() {
            continue;
        }
        let prefix = source_path.get(..start)?.trim_end_matches(['/', '\\']);
        let prefix = if prefix.is_empty() {
            source_path
                .chars()
                .next()
                .filter(|character| *character == '/' || *character == '\\')
                .map_or_else(String::new, |character| character.to_string())
        } else {
            prefix.to_string()
        };
        if mapping_path_exists(root, Some(&prefix), local_prefix, source_path) {
            return Some(prefix);
        }
    }
    None
}

fn mapped_file(root: &std::path::Path, candidate: PathBuf) -> Option<PathBuf> {
    let path = std::fs::canonicalize(candidate).ok()?;
    (path.starts_with(root) && path.is_file()).then_some(path)
}

#[cfg(test)]
mod live_acquisition_tests {
    use super::*;

    #[tokio::test]
    async fn local_search_is_unavailable_not_successfully_empty() {
        let directory = tempfile::tempdir().unwrap();
        let local =
            crate::file::local::LocalSource::from_roots(vec![directory.path().to_path_buf()])
                .unwrap();
        let source = Source::new(
            SourceId::new(crate::file::local::LOCAL_LIBRARY_SOURCE_ID),
            Implementation::Local(local),
        );
        assert!(matches!(
            source
                .live_search(
                    &Catalog::open(directory.path().join("search.db"))
                        .await
                        .unwrap(),
                    "track",
                    20,
                    None
                )
                .await,
            Err(SourceError::InvalidRequest(_))
        ));
    }
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
    async fn reload_infers_one_mapping_and_preserves_access_when_a_location_is_unavailable() {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().join("mounted-share");
        std::fs::create_dir_all(root.join("Artist")).unwrap();
        for name in ["one", "two", "other"] {
            std::fs::write(root.join("Artist").join(format!("{name}.flac")), b"media").unwrap();
        }
        let catalog = Catalog::open(directory.path().join("library.sqlite"))
            .await
            .unwrap();
        let mut updates = catalog.changes();
        let source = Source::new(
            SourceId::new("mapping-test"),
            Implementation::Local(
                crate::file::local::LocalSource::from_roots(vec![root.clone()]).unwrap(),
            ),
        );
        catalog
            .reconcile_source(&library::SourceId::new("mapping-test"), "server", "Music")
            .await
            .unwrap();
        let catalog_source_id = ("mapping-test").to_string();
        let mut observation = catalog.observe(&catalog_source_id).await.unwrap();
        for (id, path) in [
            ("one", "/server/music/Artist/one.flac"),
            ("two", "/server/music/Artist/two.flac"),
            ("other", "/different/root/Artist/other.flac"),
        ] {
            observation
                .apply_catalog(
                    &library::CatalogBatch {
                        tracks: (&[library::TrackObservation {
                            metadata: library::TrackMetadata {
                                title: id.to_owned(),
                                album: "Album".into(),
                                artist: "Artist".into(),
                                sort_text: id.to_owned(),
                                duration_millis: 1000,
                                disc_number: 1,
                                track_number: 1,
                                source_format: Some("flac".into()),
                                ..Default::default()
                            },
                            user_data: library::NativeUserState {
                                favorite: Some(false),
                                ..Default::default()
                            },
                            object_id: id.to_owned(),
                            source_path: Some(path.to_owned()),
                            ..Default::default()
                        }])
                            .to_vec(),
                        ..Default::default()
                    },
                    true,
                )
                .await
                .unwrap();
        }
        let publication = crate::published(&mut updates).expect("catalog update");
        assert!(publication.1.intersects(sources::Shown::CATALOG));
        let sample = Some("/server/music/Artist/one.flac");
        let (mapped_root, prefix) = source
            .check_local_mapping(&catalog, publication.0, &root, None, None, sample)
            .await
            .unwrap();
        assert_eq!(prefix.as_deref(), Some("/server/music"));
        catalog.set_local_access(
            [(
                "mapping-test".to_owned(),
                library::LocalAccess {
                    root: mapped_root.to_string_lossy().into_owned(),
                    server_prefix: prefix.clone(),
                    local_prefix: None,
                },
            )]
            .into(),
        );
        let tracks = catalog
            .mapping_track_page(
                publication.0,
                None,
                None,
                3,
                &library::ReadCancellation::new(),
            )
            .await
            .unwrap();
        for track in &tracks {
            assert_eq!(
                crate::playable(&catalog, &track.media_uri)
                    .await
                    .mapped
                    .and_then(|mapped| mapped.locate())
                    .is_some(),
                track.media_uri
                    != library::source_entity_uri(&SourceId::new("mapping-test"), "track", "other")
            );
        }
        assert!(
            source
                .check_local_mapping(
                    &catalog,
                    publication.0,
                    &directory.path().join("unavailable-share"),
                    prefix.as_deref(),
                    None,
                    sample,
                )
                .await
                .is_err()
        );
        assert!(
            source
                .check_local_mapping(
                    &catalog,
                    publication.0,
                    &root,
                    Some("/wrong/prefix"),
                    None,
                    sample,
                )
                .await
                .is_err()
        );
        for track in &tracks {
            assert_eq!(
                crate::playable(&catalog, &track.media_uri)
                    .await
                    .mapped
                    .and_then(|mapped| mapped.locate())
                    .is_some(),
                track.media_uri
                    != library::source_entity_uri(&SourceId::new("mapping-test"), "track", "other")
            );
        }
    }

    #[tokio::test]
    async fn cancelled_jellyfin_feed_releases_a_pending_connection() {
        use tokio::io::AsyncReadExt;
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let (started, pending) = tokio::sync::oneshot::channel();
        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut bytes = [0; 4096];
            assert!(stream.read(&mut bytes).await.unwrap() > 0);
            let _ = started.send(());
            while stream.read(&mut bytes).await.unwrap() > 0 {}
        });
        let source = Arc::new(Source::open(SourceConfiguration {
            source_id: SourceId::new("retired-jellyfin"), kind:"jellyfin".into(), name:"Music".into(),
            provider_payload:serde_json::json!({"version":1,"base_url":format!("http://{address}"),"server_id":"server","user_id":"user","username":"listener","trust_invalid_cert":false,"use_jellyfin_instant_mix":false}).to_string(),
        }, Some("token".into()), Some("device".into()), None).unwrap());
        let weak = Arc::downgrade(&source);
        let feed = source.start_selected_feed(&tokio::runtime::Handle::current(), false);
        tokio::time::timeout(std::time::Duration::from_secs(2), pending)
            .await
            .unwrap()
            .unwrap();
        feed.cancel();
        drop(feed);
        drop(source);
        tokio::time::timeout(std::time::Duration::from_secs(2), server)
            .await
            .unwrap()
            .unwrap();
        assert!(weak.upgrade().is_none());
    }

    #[test]
    fn selected_feed_accumulates_exact_jellyfin_ids_with_one_bound() {
        let merged = SelectedFeedChange::Remote(RemoteItemChange::items(
            vec!["two".to_string(), "one".to_string()],
            Vec::new(),
        ))
        .merge(SelectedFeedChange::Remote(RemoteItemChange::items(
            vec!["one".to_string(), "three".to_string()],
            vec!["gone".to_string()],
        )));
        let SelectedFeedChange::Remote(RemoteItemChange::Items(items)) = merged else {
            panic!("exact evidence widened unexpectedly");
        };
        assert_eq!(
            items,
            BTreeMap::from([
                ("one".into(), RemoteItemUpdate::Metadata),
                ("three".into(), RemoteItemUpdate::Metadata),
                ("two".into(), RemoteItemUpdate::Metadata),
                ("gone".into(), RemoteItemUpdate::Remove),
            ])
        );
    }

    #[test]
    fn selected_feed_overflow_coalesces_to_one_boundary_operation() {
        let merged = SelectedFeedChange::Remote(RemoteItemChange::items(
            (0..LIVE_CHANGE_LIMIT)
                .map(|index| format!("item-{index}"))
                .collect(),
            Vec::new(),
        ))
        .merge(SelectedFeedChange::Remote(RemoteItemChange::items(
            vec!["overflow".to_string()],
            Vec::new(),
        )));
        assert!(matches!(
            merged,
            SelectedFeedChange::Remote(RemoteItemChange::BoundaryLost)
        ));
    }

    #[test]
    fn repeated_boundary_evidence_admits_one_recovery() {
        let (wake, receiver) = async_channel::bounded(1);
        let mut pending = Some(SelectedFeedChange::Remote(RemoteItemChange::BoundaryLost));
        pending = Some(
            pending
                .take()
                .expect("first boundary")
                .merge(SelectedFeedChange::Remote(RemoteItemChange::BoundaryLost)),
        );
        assert!(wake.try_send(()).is_ok());
        let _ = wake.try_send(());
        assert!(matches!(
            pending.as_ref(),
            Some(SelectedFeedChange::Remote(RemoteItemChange::BoundaryLost))
        ));
        assert!(receiver.try_recv().is_ok());
        assert!(receiver.try_recv().is_err());
    }

    #[test]
    fn one_prefix_formula_projects_jellyfin_navidrome_and_subsonic_paths() {
        let root = tempfile::tempdir().expect("local music root");
        let track = root.path().join("Artist").join("Album").join("Track.flac");
        std::fs::create_dir_all(track.parent().expect("Track parent")).expect("create Album");
        std::fs::write(&track, b"media").expect("write Track");
        let canonical_root = root.path().canonicalize().expect("canonical music root");
        let canonical_track = track.canonicalize().expect("canonical Track");

        assert_eq!(
            mapped_local_path(
                &canonical_root,
                Some("/srv/navidrome/music"),
                None,
                "/srv/navidrome/music/Artist/Album/Track.flac",
            ),
            Some(canonical_track.clone())
        );
        assert_eq!(
            match_local_access_sample(
                &canonical_root,
                None,
                None,
                "/srv/navidrome/music/Artist/Album/Track.flac",
            ),
            Some(Some("/srv/navidrome/music".to_string()))
        );
        assert_eq!(
            library::project_local_access_path(
                &canonical_root,
                Some("/media/music"),
                None,
                "/media/music/Artist/Album/Track.flac",
            ),
            Some(canonical_track.clone())
        );
        assert_eq!(
            library::project_local_access_path(
                &canonical_root,
                None,
                None,
                "Artist/Album/Track.flac",
            ),
            Some(canonical_track)
        );
        assert_eq!(
            match_local_access_sample(&canonical_root, None, None, "Artist/Album/Track.flac",),
            Some(None)
        );
    }

    #[test]
    fn mapping_without_a_prefix_does_not_search_for_a_different_formula_per_track() {
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
                "/different/server/root/Artist/Album/Track.flac",
            ),
            None
        );
    }
}
