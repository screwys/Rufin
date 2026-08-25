//! Connects configured providers to Library's concrete Scan and point operations.
//! Provider acquisition remains here; accepted catalog identity and queries remain in Library.

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use library::{Database, Freshness, Scan, ScanOutcome};
use playback::{ResolvedStream, SourceReportFact, StreamRequest};
use serde::{Deserialize, Serialize};

use crate::{
    ImageBytes, SourceConfiguration, SourceError, SourceId, SourceResult, SourceSettingsInput,
    SourceSetupInput,
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
    pub tracks: Vec<LiveSearchTrack>,
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

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum JellyfinLiveChange {
    Items {
        upserts: Vec<String>,
        removals: Vec<String>,
    },
    BoundaryLost,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum LocalLiveChange {
    Paths(Vec<PathBuf>),
    Rescan,
}

impl LocalLiveChange {
    pub(crate) fn merge(&mut self, incoming: Self) -> Result<(), Self> {
        match (&mut *self, incoming) {
            (Self::Rescan, _) => Ok(()),
            (current, Self::Rescan) => {
                *current = Self::Rescan;
                Ok(())
            }
            (Self::Paths(current), Self::Paths(mut incoming)) => {
                current.append(&mut incoming);
                current.sort();
                current.dedup();
                Ok(())
            }
        }
    }
}

pub enum RemotePlaylistEdit {
    Create {
        name: String,
        track_object_ids: Vec<String>,
    },
    Rename {
        playlist_object_id: String,
        name: String,
    },
    Delete {
        playlist_object_id: String,
    },
    Add {
        playlist_object_id: String,
        track_object_ids: Vec<String>,
    },
    Remove {
        playlist_object_id: String,
        occurrence_ids: Vec<String>,
    },
    Move {
        playlist_object_id: String,
        occurrence_id: String,
        position: usize,
    },
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

pub(crate) trait RemotePlaylistSource {
    async fn create_playlist(&self, name: &str, track_ids: &[String]) -> SourceResult<String>;
    async fn rename_playlist(&self, playlist_id: &str, name: &str) -> SourceResult<()>;
    async fn delete_playlist(&self, playlist_id: &str) -> SourceResult<()>;
    async fn add_playlist_tracks(
        &self,
        playlist_id: &str,
        track_ids: &[String],
    ) -> SourceResult<()>;
    async fn remove_playlist_entries(
        &self,
        playlist_id: &str,
        occurrence_ids: &[String],
    ) -> SourceResult<()>;
    async fn move_playlist_entry(
        &self,
        playlist_id: &str,
        occurrence_id: &str,
        new_index: usize,
    ) -> SourceResult<()>;
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
    ) -> SourceResult<ScanOutcome> {
        self.refresh(database, display_name, progress, cancelled, true)
            .await
    }

    pub async fn manual_refresh(
        &self,
        database: &Database,
        display_name: &str,
        progress: &(dyn Fn(SourceReadProgress) + Send + Sync),
        cancelled: Arc<AtomicBool>,
    ) -> SourceResult<ScanOutcome> {
        self.refresh(database, display_name, progress, cancelled, false)
            .await
    }

    async fn refresh(
        &self,
        database: &Database,
        display_name: &str,
        progress: &(dyn Fn(SourceReadProgress) + Send + Sync),
        cancelled: Arc<AtomicBool>,
        accept_freshness: bool,
    ) -> SourceResult<ScanOutcome> {
        let freshness = self.freshness().await?;
        if accept_freshness && let Some(freshness) = freshness.as_ref() {
            let cancellation = library::ReadCancellation::new();
            if cancelled.load(Ordering::Relaxed) {
                cancellation.cancel();
            }
            if let Some(publication) =
                Scan::accept_freshness(database, self.source_id.as_str(), freshness, &cancellation)
                    .await?
            {
                return Ok(ScanOutcome::Identical(publication));
            }
        }
        let mut scan = Scan::begin(
            database,
            self.source_id.as_str(),
            display_name,
            &display_name.to_lowercase(),
            freshness,
        )
        .await?;
        let initial_local = matches!(&self.implementation, Implementation::Local(_))
            && scan.existing_source().is_none();
        let cancelled_fn = || cancelled.load(Ordering::Relaxed);
        match &self.implementation {
            Implementation::Local(source) => {
                source
                    .stage_catalog(database, &mut scan, progress, &cancelled_fn)
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
        if let Implementation::Local(source) = &self.implementation
            && let ScanOutcome::Changed(publication) = outcome
        {
            if initial_local {
                source
                    .persist_initial_observations(database, publication.source, &cancelled_fn)
                    .await?;
            }
            source
                .reconcile_observations(database, publication.source)
                .await?;
            return Ok(ScanOutcome::Changed(publication));
        }
        Ok(outcome)
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
            Implementation::Local(_) => Ok(LiveSearchResults::default()),
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

    fn metadata_account_available(&self) -> bool {
        match &self.implementation {
            Implementation::Local(_) => true,
            Implementation::Jellyfin(source) => source.metadata_editing_available(),
            Implementation::OpenSubsonic(source) => source.metadata_editing_available(),
        }
    }

    pub async fn refresh_metadata_editing(&self) {
        match &self.implementation {
            Implementation::Local(_) => {}
            Implementation::Jellyfin(_) => {}
            Implementation::OpenSubsonic(source) => source.refresh_metadata_editing().await,
        }
    }

    pub async fn refresh_remote_metadata_index(&self) -> SourceResult<()> {
        match &self.implementation {
            Implementation::Local(_) | Implementation::Jellyfin(_) => Ok(()),
            Implementation::OpenSubsonic(source) => {
                source.require_metadata_scan_idle().await?;
                source.start_metadata_scan_and_wait().await
            }
        }
    }

    pub async fn track_metadata_available(
        &self,
        database: &Database,
        source: library::SourceKey,
        track_key: library::TrackKey,
    ) -> SourceResult<bool> {
        if matches!(&self.implementation, Implementation::Jellyfin(_)) {
            return self
                .jellyfin_track_metadata_available(database, source, track_key)
                .await;
        }
        let Some((path, format)) = self
            .metadata_track_path(database, source, track_key)
            .await?
        else {
            return Ok(false);
        };
        if matches!(&self.implementation, Implementation::OpenSubsonic(_))
            && !crate::local::basic_metadata_readable(path.clone())
        {
            return Ok(false);
        }
        Ok(crate::local::metadata_file_available(
            &path,
            format.as_deref(),
        ))
    }

    pub async fn read_track_metadata(
        &self,
        database: &Database,
        source: library::SourceKey,
        track_key: library::TrackKey,
    ) -> Result<crate::TrackMetadata, crate::SourceMetadataError> {
        if let Implementation::Jellyfin(jellyfin) = &self.implementation {
            let cancellation = library::ReadCancellation::new();
            let track = database
                .track_rows(source, &[track_key], &cancellation)
                .await
                .map_err(|error| crate::SourceMetadataError::Write(error.to_string()))?
                .pop()
                .ok_or(crate::SourceMetadataError::Unavailable)?;
            return jellyfin.read_track_metadata(track).await;
        }
        let (path, format) = self
            .metadata_track_path(database, source, track_key)
            .await
            .map_err(|error| crate::SourceMetadataError::Write(error.to_string()))?
            .ok_or(crate::SourceMetadataError::Unavailable)?;
        crate::local::read_track_metadata(track_key, &path, format.as_deref())
    }

    async fn jellyfin_track_metadata_available(
        &self,
        database: &Database,
        source: library::SourceKey,
        track_key: library::TrackKey,
    ) -> SourceResult<bool> {
        let Implementation::Jellyfin(jellyfin) = &self.implementation else {
            return Ok(false);
        };
        let cancellation = library::ReadCancellation::new();
        let Some(track) = database
            .track_rows(source, &[track_key], &cancellation)
            .await?
            .pop()
        else {
            return Ok(false);
        };
        Ok(jellyfin.read_track_metadata(track).await.is_ok())
    }

    pub async fn album_metadata_available(
        &self,
        database: &Database,
        source: library::SourceKey,
        album_key: library::AlbumKey,
    ) -> SourceResult<bool> {
        let Implementation::Jellyfin(jellyfin) = &self.implementation else {
            return Ok(self
                .read_album_metadata(database, source, album_key)
                .await
                .is_ok());
        };
        let cancellation = library::ReadCancellation::new();
        let Some(album) = database
            .album_rows(source, &[album_key], None, &cancellation)
            .await?
            .pop()
        else {
            return Ok(false);
        };
        Ok(jellyfin.read_album_metadata(album).await.is_ok())
    }

    pub async fn artist_metadata_available(
        &self,
        database: &Database,
        source: library::SourceKey,
        artist_key: library::ArtistKey,
    ) -> SourceResult<bool> {
        let Implementation::Jellyfin(jellyfin) = &self.implementation else {
            return Ok(self
                .read_artist_metadata(database, source, artist_key)
                .await
                .is_ok());
        };
        let cancellation = library::ReadCancellation::new();
        let Some(artist) = database
            .artist_rows(source, &[artist_key], false, None, &cancellation)
            .await?
            .pop()
        else {
            return Ok(false);
        };
        Ok(jellyfin.read_artist_metadata(artist).await.is_ok())
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
                self.read_mapped_album_metadata(database, source, album)
                    .await
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
                self.read_mapped_artist_metadata(database, source, artist)
                    .await
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
        values: crate::TrackMetadataValues,
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
                        |item| super::jellyfin::metadata::apply_track_values(item, &values),
                    )
                    .await?;
                self.publish_jellyfin_metadata(database, raw).await?;
            }
            Implementation::Local(_) | Implementation::OpenSubsonic(_) => {
                self.write_file_track_metadata(
                    database,
                    source,
                    &track,
                    expected_revision,
                    &values,
                )
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
        values: crate::AlbumMetadataValues,
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
                        |item| super::jellyfin::metadata::apply_album_values(item, &values),
                    )
                    .await?;
                self.publish_jellyfin_metadata(database, raw).await?;
            }
            Implementation::Local(_) | Implementation::OpenSubsonic(_) => {
                self.write_file_album_metadata(
                    database,
                    source,
                    &album,
                    expected_revision,
                    &values,
                )
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
        values: crate::ArtistMetadataValues,
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
                        |item| super::jellyfin::metadata::apply_artist_values(item, &values),
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
                    &values,
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

    async fn read_mapped_album_metadata(
        &self,
        database: &Database,
        source: library::SourceKey,
        album: library::AlbumRow,
    ) -> Result<crate::AlbumMetadata, crate::SourceMetadataError> {
        if !self.metadata_account_available() {
            return Err(crate::SourceMetadataError::Unavailable);
        }
        self.read_local_album_metadata(database, source, album)
            .await
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

    async fn read_mapped_artist_metadata(
        &self,
        database: &Database,
        source: library::SourceKey,
        artist: library::ArtistRow,
    ) -> Result<crate::ArtistMetadata, crate::SourceMetadataError> {
        if !self.metadata_account_available() {
            return Err(crate::SourceMetadataError::Unavailable);
        }
        self.read_local_artist_metadata(database, source, artist)
            .await
    }

    async fn write_file_track_metadata(
        &self,
        database: &Database,
        source: library::SourceKey,
        track: &library::TrackRow,
        expected_revision: &str,
        values: &crate::TrackMetadataValues,
    ) -> Result<(), crate::SourceMetadataError> {
        let (path, format) = self
            .metadata_track_path(database, source, track.track_key)
            .await
            .map_err(|error| crate::SourceMetadataError::Write(error.to_string()))?
            .ok_or(crate::SourceMetadataError::Unavailable)?;
        crate::local::metadata::write_track(&path, format.as_deref(), expected_revision, values)?;
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
                        track_write_from_values(track, values),
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
        values: &crate::AlbumMetadataValues,
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
        crate::local::metadata::write_album_batch(&file_targets, expected_revision, values)?;
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
                            title: values.title.clone(),
                            normalized_title: values.title.to_lowercase(),
                            display_artist: values
                                .album_artist
                                .clone()
                                .unwrap_or_else(|| album.display_artist.clone()),
                            sort_text: values
                                .sort_title
                                .clone()
                                .unwrap_or_else(|| values.title.to_lowercase()),
                            year: values.year.map(i64::from),
                            release_date: album.release_date.clone(),
                            date_added: album.date_added.clone(),
                            musicbrainz_release_id: values.musicbrainz_album_id.clone(),
                            musicbrainz_release_group_id: values
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
        values: &crate::ArtistMetadataValues,
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
            values,
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
                            name: values.name.clone(),
                            normalized_name: values.name.to_lowercase(),
                            sort_text: values
                                .sort_name
                                .clone()
                                .unwrap_or_else(|| values.name.to_lowercase()),
                            musicbrainz_artist_id: values.musicbrainz_artist_id.clone(),
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
        for key in keys {
            let (path, format) = self
                .metadata_track_path(database, source, *key)
                .await
                .map_err(|error| crate::SourceMetadataError::Write(error.to_string()))?
                .ok_or(crate::SourceMetadataError::Unavailable)?;
            if !crate::local::metadata_file_available(&path, format.as_deref()) {
                return Err(crate::SourceMetadataError::Unavailable);
            }
            targets.push(MetadataFileTarget {
                track_key: *key,
                path,
                format,
            });
        }
        targets.sort_by(|left, right| left.path.cmp(&right.path));
        targets.dedup_by(|left, right| left.path == right.path);
        Ok(targets)
    }

    async fn metadata_track_path(
        &self,
        database: &Database,
        source: library::SourceKey,
        track_key: library::TrackKey,
    ) -> SourceResult<Option<(PathBuf, Option<String>)>> {
        let cancellation = library::ReadCancellation::new();
        let Some(track) = database
            .track_rows(source, &[track_key], &cancellation)
            .await?
            .pop()
        else {
            return Ok(None);
        };
        if track.cue_path.is_some() {
            return Ok(None);
        }
        if let Some(path) = track
            .media_uri
            .as_deref()
            .and_then(|uri| uri.strip_prefix("file://"))
        {
            return Ok(Some((PathBuf::from(path), track.source_format)));
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
        {
            return Ok(Some((PathBuf::from(access.path), track.source_format)));
        }
        Ok(None)
    }

    pub async fn edit_remote_playlist(
        &self,
        edit: RemotePlaylistEdit,
    ) -> SourceResult<Option<String>> {
        match &self.implementation {
            Implementation::Local(_) => Err(SourceError::InvalidRequest(
                "Local playlists are Library-owned",
            )),
            Implementation::Jellyfin(source) => apply_playlist_edit(source, edit).await,
            Implementation::OpenSubsonic(source) => apply_playlist_edit(source, edit).await,
        }
    }

    pub async fn lyrics(
        &self,
        track_object_id: &str,
        search: crate::LyricsSearch,
    ) -> SourceResult<Option<crate::NativeLyrics>> {
        match &self.implementation {
            Implementation::Local(_) => Ok(None),
            Implementation::Jellyfin(source) => source.lyrics(track_object_id, search).await,
            Implementation::OpenSubsonic(source) => source.lyrics(track_object_id, search).await,
        }
    }

    pub async fn browse_folder(
        &self,
        folder_object_id: Option<&str>,
        music_folder_object_id: Option<&str>,
    ) -> SourceResult<LiveFolderPage> {
        match &self.implementation {
            Implementation::Local(_) => Ok(LiveFolderPage::default()),
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

    pub async fn listen_jellyfin_changes(
        &self,
        on_ready: &mut (dyn FnMut() -> bool + Send),
        on_gap: &mut (dyn FnMut() -> bool + Send),
        on_change: &mut (dyn FnMut(JellyfinLiveChange) -> bool + Send),
    ) -> SourceResult<()> {
        match &self.implementation {
            Implementation::Jellyfin(source) => {
                source
                    .listen_library_changes(on_ready, on_gap, on_change)
                    .await
            }
            _ => Err(SourceError::InvalidRequest(
                "Jellyfin observer requested for another source",
            )),
        }
    }

    pub async fn apply_jellyfin_change(
        &self,
        database: &Database,
        change: JellyfinLiveChange,
    ) -> SourceResult<Option<ScanOutcome>> {
        match (&self.implementation, change) {
            (Implementation::Jellyfin(_), JellyfinLiveChange::BoundaryLost) => Ok(None),
            (Implementation::Jellyfin(source), JellyfinLiveChange::Items { upserts, removals }) => {
                source
                    .apply_live_items(database, self.source_id.as_str(), upserts, removals)
                    .await
                    .map(Some)
            }
            _ => Err(SourceError::InvalidRequest(
                "Jellyfin change belongs to another source",
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

    pub fn listen_local_changes(
        &self,
        on_ready: &mut dyn FnMut(bool) -> bool,
        on_change: &mut dyn FnMut(LocalLiveChange) -> bool,
        should_stop: &dyn Fn() -> bool,
    ) -> SourceResult<()> {
        match &self.implementation {
            Implementation::Local(source) => source.watch(on_ready, on_change, should_stop),
            _ => Err(SourceError::InvalidRequest(
                "Local observer requested for another source",
            )),
        }
    }

    pub async fn report_playback(&self, report: &SourceReportFact) -> SourceResult<()> {
        match &self.implementation {
            Implementation::Local(_) => Ok(()),
            Implementation::Jellyfin(source) => source.report_playback(report).await,
            Implementation::OpenSubsonic(source) => source.report_playback(report).await,
        }
    }
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
    let mut mixed = crate::AlbumMetadataMixed::default();
    for target in &targets[1..] {
        let observed =
            crate::local::read_album_metadata_values(&target.path, target.format.as_deref())?;
        mixed.title |= observed.title != values.title;
        mixed.sort_title |= observed.sort_title != values.sort_title;
        mixed.album_artist |= observed.album_artist != values.album_artist;
        mixed.year |= observed.year != values.year;
        mixed.genre |= observed.genre != values.genre;
        mixed.comment |= observed.comment != values.comment;
        mixed.musicbrainz_album_id |= observed.musicbrainz_album_id != values.musicbrainz_album_id;
        mixed.musicbrainz_release_group_id |=
            observed.musicbrainz_release_group_id != values.musicbrainz_release_group_id;
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
        values,
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
    let values = crate::local::read_artist_metadata_values(
        &first.path,
        first.format.as_deref(),
        &artist.name,
    )?;
    let mut mixed = crate::ArtistMetadataMixed::default();
    for target in &targets[1..] {
        let observed = crate::local::read_artist_metadata_values(
            &target.path,
            target.format.as_deref(),
            &artist.name,
        )?;
        mixed.sort_name |= observed.sort_name != values.sort_name;
        mixed.genre |= observed.genre != values.genre;
        mixed.comment |= observed.comment != values.comment;
        mixed.musicbrainz_artist_id |=
            observed.musicbrainz_artist_id != values.musicbrainz_artist_id;
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
        values,
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

fn metadata_database_error(error: library::LibraryError) -> crate::SourceMetadataError {
    crate::SourceMetadataError::Write(error.to_string())
}

async fn apply_playlist_edit(
    source: &impl RemotePlaylistSource,
    edit: RemotePlaylistEdit,
) -> SourceResult<Option<String>> {
    match edit {
        RemotePlaylistEdit::Create {
            name,
            track_object_ids,
        } => source
            .create_playlist(&name, &track_object_ids)
            .await
            .map(Some),
        RemotePlaylistEdit::Rename {
            playlist_object_id,
            name,
        } => {
            source.rename_playlist(&playlist_object_id, &name).await?;
            Ok(Some(playlist_object_id))
        }
        RemotePlaylistEdit::Delete { playlist_object_id } => {
            source.delete_playlist(&playlist_object_id).await?;
            Ok(None)
        }
        RemotePlaylistEdit::Add {
            playlist_object_id,
            track_object_ids,
        } => {
            source
                .add_playlist_tracks(&playlist_object_id, &track_object_ids)
                .await?;
            Ok(Some(playlist_object_id))
        }
        RemotePlaylistEdit::Remove {
            playlist_object_id,
            occurrence_ids,
        } => {
            source
                .remove_playlist_entries(&playlist_object_id, &occurrence_ids)
                .await?;
            Ok(Some(playlist_object_id))
        }
        RemotePlaylistEdit::Move {
            playlist_object_id,
            occurrence_id,
            position,
        } => {
            source
                .move_playlist_entry(&playlist_object_id, &occurrence_id, position)
                .await?;
            Ok(Some(playlist_object_id))
        }
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
