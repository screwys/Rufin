//! Local filesystem source.
//!
//! Local owns walking, tags, CUE interpretation, and artwork locators. It
//! produces canonical facts and inert change plans; it never retains another
//! queryable music library beside Library's selected collection.

use std::fs;
use std::path::{Path, PathBuf};

use lofty::prelude::{Accessor, TaggedFileExt};
use lofty::tag::ItemKey;
use serde::Deserialize;
use walkdir::WalkDir;

use crate::source::SourceReadProgress;
use crate::{
    ConnectedSource, ImageBytes, LocalFolderHostInput, SourceConfiguration, SourceEditResult,
    SourceError, SourceResult,
};

mod artwork;
mod cue;
mod discovery;
mod lofty_metadata;
mod media;
pub(crate) mod metadata;
mod scan;
mod watch;

pub const LOCAL_SOURCE_ID: &str = "local";
pub const LOCAL_LIBRARY_SOURCE_ID: &str = "local:server:library";
const SOURCE_CONFIG_VERSION: u32 = 1;

#[derive(Deserialize)]
struct LocalSourcePayload {
    version: u32,
    #[serde(default)]
    roots: Vec<String>,
    #[serde(default, alias = "base_url")]
    legacy_root: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LocalSourceConfig {
    pub roots: Vec<PathBuf>,
}

impl LocalSourceConfig {
    pub fn from_configuration(stored: &SourceConfiguration) -> SourceResult<Self> {
        if stored.kind != LOCAL_SOURCE_ID {
            return Err(SourceError::InvalidConfig(format!(
                "expected {LOCAL_SOURCE_ID}, found {}",
                stored.kind
            )));
        }
        let payload: LocalSourcePayload = crate::config::decode_provider_payload(stored)?;
        crate::config::require_payload_version(payload.version, SOURCE_CONFIG_VERSION)?;
        let mut roots = payload
            .roots
            .into_iter()
            .filter(|root| !root.trim().is_empty())
            .map(PathBuf::from)
            .collect::<Vec<_>>();
        if roots.is_empty()
            && let Some(root) = payload.legacy_root.filter(|root| !root.trim().is_empty())
        {
            roots.push(PathBuf::from(root));
        }
        if roots.is_empty() {
            return Err(SourceError::InvalidConfig(
                "a Local source must contain at least one folder".to_string(),
            ));
        }
        Ok(Self { roots })
    }

    pub(crate) fn into_payload(self) -> serde_json::Value {
        let roots = self
            .roots
            .iter()
            .map(|root| root.to_string_lossy().into_owned())
            .collect::<Vec<_>>();
        serde_json::json!({
            "version": SOURCE_CONFIG_VERSION,
            "roots": roots,
        })
    }
}

#[derive(Debug)]
pub struct LocalSource {
    roots: Vec<PathBuf>,
}

impl LocalSource {
    pub fn from_configuration(configuration: &SourceConfiguration) -> SourceResult<Self> {
        let config = LocalSourceConfig::from_configuration(configuration)?;
        let roots = configured_roots(config.roots)?;
        Ok(Self { roots })
    }

    pub fn from_roots(roots: Vec<PathBuf>) -> SourceResult<Self> {
        let roots = normalize_roots(roots)?;
        Ok(Self { roots })
    }

    pub fn roots(&self) -> &[PathBuf] {
        &self.roots
    }

    pub(super) async fn stage_catalog(
        &self,
        database: &library::Database,
        scan: &mut library::Scan,
        progress: &(dyn Fn(SourceReadProgress) + Send + Sync),
        cancelled: &(dyn Fn() -> bool + Send + Sync),
        reuse_unchanged: bool,
    ) -> SourceResult<()> {
        scan::stage_catalog(
            database,
            &self.roots,
            scan,
            progress,
            cancelled,
            reuse_unchanged,
        )
        .await
    }

    pub(super) async fn publish_metadata_paths(
        &self,
        database: &library::Database,
        source_id: &str,
        paths: &[PathBuf],
        removed_album: Option<&str>,
        removed_artist: Option<&str>,
    ) -> SourceResult<library::ScanOutcome> {
        scan::publish_metadata_paths(database, source_id, paths, removed_album, removed_artist)
            .await
    }

    pub(super) async fn publish_paths(
        &self,
        database: &library::Database,
        source: library::SourceKey,
        source_id: &str,
        paths: &[PathBuf],
        rename: Option<&(PathBuf, PathBuf)>,
    ) -> SourceResult<library::ScanOutcome> {
        scan::publish_paths(database, source, source_id, &self.roots, paths, rename).await
    }

    pub(super) async fn catch_up(
        &self,
        database: &library::Database,
        source: library::SourceKey,
        source_id: &str,
        progress: &(dyn Fn(crate::SourceReadProgress) + Send + Sync),
        cancelled: &(dyn Fn() -> bool + Send + Sync),
    ) -> SourceResult<library::ScanOutcome> {
        scan::catch_up(
            database,
            source,
            source_id,
            &self.roots,
            progress,
            cancelled,
        )
        .await
    }

    pub(crate) fn image_bytes(&self, artwork: &crate::LocalImageRef) -> SourceResult<ImageBytes> {
        let reference = match artwork {
            crate::LocalImageRef::File { path, .. } => {
                self::artwork::ArtworkReference::File(PathBuf::from(path))
            }
            crate::LocalImageRef::Embedded {
                path,
                picture_index,
                ..
            } => self::artwork::ArtworkReference::Embedded {
                path: PathBuf::from(path),
                picture_index: *picture_index,
            },
        };
        if !self
            .roots
            .iter()
            .any(|root| reference.path().starts_with(root))
        {
            return Err(SourceError::NotFound);
        }
        artwork::read_image(&reference)
    }

    pub(crate) fn image(&self, request: crate::SourceImageRequest) -> SourceResult<ImageBytes> {
        match request {
            crate::SourceImageRequest::Local(reference) => self.image_bytes(&reference),
            crate::SourceImageRequest::Native { .. } => Err(SourceError::NotFound),
        }
    }

    pub(crate) async fn prepare_artwork_page(
        &self,
        database: &library::Database,
        source: library::SourceKey,
        after: Option<library::AlbumKey>,
    ) -> SourceResult<crate::ArtworkPreparationProgress> {
        let cancellation = library::ReadCancellation::new();
        let candidates = database
            .local_album_artwork_page(source, after, 128, &cancellation)
            .await?;
        let mut discoverer = discovery::Reader::default();
        let mut completed = 0;
        let mut next_album = None;
        let mut writes = Vec::with_capacity(candidates.len());
        for candidate in candidates {
            next_album = Some(candidate.album_key);
            let path = PathBuf::from(
                candidate
                    .media_uri
                    .strip_prefix("file://")
                    .unwrap_or(&candidate.media_uri),
            );
            let media_revision =
                observation_revision_values(candidate.media_size_bytes, candidate.media_mtime_ns);
            let sidecar = candidate.sidecar_path.map(|path| {
                artwork::file_reference(
                    &PathBuf::from(path),
                    observation_revision_values(
                        candidate.sidecar_size_bytes,
                        candidate.sidecar_mtime_ns.unwrap_or_default(),
                    ),
                )
            });
            let binding = sidecar
                .or_else(|| artwork::inspect_embedded(&mut discoverer, &path, media_revision))
                .map(|binding| serde_json::to_vec(&binding))
                .transpose()?
                .unwrap_or_else(|| br#"{"no_art":true}"#.to_vec());
            writes.push((candidate.album_key, binding));
            completed += 1;
        }
        database
            .write_album_artwork_bindings(source, &writes)
            .await?;
        Ok(crate::ArtworkPreparationProgress {
            completed,
            next_album,
        })
    }

    pub(crate) fn watch(
        &self,
        on_ready: &mut dyn FnMut(bool) -> bool,
        on_change: &mut dyn FnMut(crate::LocalLiveChange) -> bool,
        should_stop: &dyn Fn() -> bool,
    ) -> SourceResult<()> {
        watch::LocalChangeFeed::new(self.roots.clone()).listen_forever(
            on_ready,
            on_change,
            should_stop,
        )
    }
}

pub(crate) async fn apply_metadata_mapping(
    database: &library::Database,
    source: library::SourceKey,
    root: &Path,
    cancelled: &(dyn Fn() -> bool + Send + Sync),
) -> SourceResult<usize> {
    let mut worker = media::Worker::default();
    let mut accepted = 0;
    for entry in WalkDir::new(root).follow_links(false) {
        if cancelled() {
            return Err(SourceError::Cancelled);
        }
        let entry = entry.map_err(|error| SourceError::Other(error.to_string()))?;
        if !entry.file_type().is_file() {
            continue;
        }
        let path = entry.into_path();
        let Some(metadata) = media::read_basic_audio(&mut worker, path.clone()) else {
            continue;
        };
        let object_id = database
            .mapping_track_object(
                source,
                &metadata.title,
                &metadata.album,
                &metadata.artist,
                i64::from(metadata.disc_number),
                i64::from(metadata.track_number),
                i64::from(metadata.duration_seconds) * 1000,
                &library::ReadCancellation::new(),
            )
            .await?;
        let Some(object_id) = object_id else { continue };
        let file = fs::metadata(&path).map_err(|error| SourceError::Other(error.to_string()))?;
        let mtime_ns = file
            .modified()
            .ok()
            .and_then(|value| value.duration_since(std::time::UNIX_EPOCH).ok())
            .and_then(|value| i64::try_from(value.as_nanos()).ok())
            .unwrap_or_default();
        #[cfg(unix)]
        let (device_id, inode) = {
            use std::os::unix::fs::MetadataExt;
            (
                i64::try_from(file.dev()).ok(),
                i64::try_from(file.ino()).ok(),
            )
        };
        #[cfg(not(unix))]
        let (device_id, inode) = (None, None);
        let media_uri = url::Url::from_file_path(&path)
            .map_err(|()| SourceError::Other("could not create mapped file URI".to_string()))?
            .to_string();
        let mut hash = blake3::Hasher::new();
        hash.update(path.to_string_lossy().as_bytes());
        hash.update(&file.len().to_le_bytes());
        hash.update(&mtime_ns.to_le_bytes());
        database
            .upsert_local_access(
                source,
                &library::LocalAccessWrite {
                    track_object_id: Some(object_id),
                    origin: library::LocalAccessOrigin::Mapping,
                    path: path.to_string_lossy().into_owned(),
                    root: root.to_string_lossy().into_owned(),
                    relative_path: path
                        .strip_prefix(root)
                        .unwrap_or(&path)
                        .to_string_lossy()
                        .into_owned(),
                    size_bytes: i64::try_from(file.len()).unwrap_or(i64::MAX),
                    mtime_ns,
                    device_id,
                    inode,
                    parser_version: 1,
                    title: metadata.title,
                    album: metadata.album,
                    artist: metadata.artist,
                    disc_number: i64::from(metadata.disc_number),
                    track_number: i64::from(metadata.track_number),
                    duration_millis: i64::from(metadata.duration_seconds) * 1000,
                    media_uri,
                    loudness_analysis_key: *hash.finalize().as_bytes(),
                },
            )
            .await?;
        accepted += 1;
    }
    Ok(accepted)
}

fn observation_revision_values(size_bytes: Option<i64>, mtime_ns: i64) -> String {
    format!("{}-{mtime_ns}", size_bytes.unwrap_or_default())
}

pub(crate) fn connect(input: LocalFolderHostInput) -> SourceResult<ConnectedSource> {
    let source = LocalSource::from_roots(input.roots)?;
    let configuration = crate::config::encode_provider_payload(
        crate::SourceId::new(LOCAL_LIBRARY_SOURCE_ID),
        LOCAL_SOURCE_ID,
        "Local",
        LocalSourceConfig {
            roots: source.roots().to_vec(),
        }
        .into_payload(),
    );
    Ok(ConnectedSource::local(configuration, source))
}

pub(crate) fn edit(
    current: SourceConfiguration,
    roots: Vec<PathBuf>,
) -> SourceResult<SourceEditResult> {
    crate::source::require_source_edit(&current, LOCAL_SOURCE_ID)?;
    let source = LocalSource::from_roots(roots)?;
    let configuration = crate::config::encode_provider_payload(
        current.source_id.clone(),
        LOCAL_SOURCE_ID,
        current.name.clone(),
        LocalSourceConfig {
            roots: source.roots().to_vec(),
        }
        .into_payload(),
    );
    if configuration == current {
        return Ok(SourceEditResult::Unchanged);
    }
    Ok(SourceEditResult::Connected(Box::new(
        ConnectedSource::local(configuration, source),
    )))
}

pub fn verify_local_media_file(path: &Path) -> SourceResult<()> {
    let path = fs::canonicalize(path).map_err(|error| {
        SourceError::Other(format!("Could not read {}: {error}", path.display()))
    })?;
    let mut worker = media::Worker::default();
    let read = media::read_media(&mut worker, path.clone(), None);
    let media::MediaRead::Accepted(scanned) = read else {
        return Err(SourceError::Other(format!(
            "Could not read {}",
            path.display()
        )));
    };
    if let Some(reference) = &scanned.local_artwork {
        let root = path.parent().ok_or(SourceError::NotFound)?.to_path_buf();
        LocalSource { roots: vec![root] }.image_bytes(reference)?;
    }
    Ok(())
}

pub(crate) fn configured_roots(roots: Vec<PathBuf>) -> SourceResult<Vec<PathBuf>> {
    let mut configured = Vec::new();
    for root in roots {
        if !root.is_absolute() {
            return Err(SourceError::InvalidConfig(format!(
                "Local music folder is not absolute: {}",
                root.display()
            )));
        }
        if !configured.iter().any(|accepted| accepted == &root) {
            configured.push(root);
        }
    }
    if configured.is_empty() {
        return Err(SourceError::InvalidConfig(
            "a Local source must contain at least one folder".to_string(),
        ));
    }
    Ok(configured)
}

fn normalize_roots(roots: Vec<PathBuf>) -> SourceResult<Vec<PathBuf>> {
    let mut normalized = Vec::new();
    for root in roots {
        let root = fs::canonicalize(&root).map_err(|error| {
            SourceError::Other(format!("Could not read {}: {error}", root.display()))
        })?;
        if !root.is_dir() {
            return Err(SourceError::Other(format!(
                "{} is not a music folder",
                root.display()
            )));
        }
        fs::read_dir(&root).map_err(|error| {
            SourceError::Other(format!("Could not read {}: {error}", root.display()))
        })?;
        if !normalized.iter().any(|accepted| accepted == &root) {
            normalized.push(root);
        }
    }
    if normalized.is_empty() {
        return Err(SourceError::InvalidRequest(
            "at least one Local music folder is required",
        ));
    }
    Ok(normalized)
}

pub(super) fn metadata_file_available(path: &Path, source_format: Option<&str>) -> bool {
    source_format
        .and_then(lofty_metadata::MetadataWriter::for_source_format)
        .or_else(|| lofty_metadata::MetadataWriter::for_path(path))
        .is_some()
}

pub(super) fn read_track_metadata(
    track_key: library::TrackKey,
    path: &Path,
    source_format: Option<&str>,
) -> Result<crate::TrackMetadata, crate::SourceMetadataError> {
    let writer = source_format
        .and_then(lofty_metadata::MetadataWriter::for_source_format)
        .or_else(|| lofty_metadata::MetadataWriter::for_path(path))
        .ok_or(crate::SourceMetadataError::Unavailable)?;
    let tagged = lofty_metadata::read_lofty_for_edit(path, writer.file_type())
        .map_err(|error| crate::SourceMetadataError::Write(error.to_string()))?
        .ok_or(crate::SourceMetadataError::Unavailable)?;
    let tag = tagged.primary_tag().or_else(|| tagged.first_tag());
    let text = |key| {
        tag.and_then(|tag| tag.get_string(key))
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(ToString::to_string)
    };
    let number = |key| text(key).and_then(|value| value.parse::<u16>().ok());
    let values = crate::TrackMetadataValues {
        title: tag
            .and_then(|tag| tag.title())
            .map(|value| value.trim().to_string())
            .unwrap_or_default(),
        sort_title: text(ItemKey::TrackTitleSortOrder),
        artist: tag
            .and_then(|tag| tag.artist())
            .map(|value| value.trim().to_string()),
        album: tag
            .and_then(|tag| tag.album())
            .map(|value| value.trim().to_string()),
        album_artist: text(ItemKey::AlbumArtist),
        track_number: tag.and_then(|tag| tag.track()).map(|value| value as u16),
        disc_number: tag.and_then(|tag| tag.disk()).map(|value| value as u16),
        year: tag.and_then(|tag| tag.date()).map(|value| value.year),
        genre: tag
            .and_then(|tag| tag.genre())
            .map(|value| value.trim().to_string()),
        comment: tag
            .and_then(|tag| tag.comment())
            .map(|value| value.trim().to_string()),
        bpm: number(ItemKey::IntegerBpm).or_else(|| number(ItemKey::Bpm)),
        locked: None,
        musicbrainz_recording_id: text(ItemKey::MusicBrainzRecordingId),
        musicbrainz_release_track_id: text(ItemKey::MusicBrainzTrackId),
        musicbrainz_album_id: text(ItemKey::MusicBrainzReleaseId),
        musicbrainz_release_group_id: text(ItemKey::MusicBrainzReleaseGroupId),
        musicbrainz_artist_id: text(ItemKey::MusicBrainzArtistId),
    };
    let can = |key| writer.metadata_key_is_writable(key);
    let writable = crate::TrackMetadataWritable {
        title: can(ItemKey::TrackTitle),
        sort_title: can(ItemKey::TrackTitleSortOrder),
        artist: can(ItemKey::TrackArtist),
        album: can(ItemKey::AlbumTitle),
        album_artist: can(ItemKey::AlbumArtist),
        track_number: can(ItemKey::TrackNumber),
        disc_number: can(ItemKey::DiscNumber),
        year: can(ItemKey::RecordingDate),
        genre: can(ItemKey::Genre),
        comment: can(ItemKey::Comment),
        bpm: lofty_metadata::bpm_key(writer.file_type().primary_tag_type()).is_some(),
        locked: false,
        musicbrainz_recording_id: can(ItemKey::MusicBrainzRecordingId),
        musicbrainz_release_track_id: can(ItemKey::MusicBrainzTrackId),
        musicbrainz_album_id: can(ItemKey::MusicBrainzReleaseId),
        musicbrainz_release_group_id: can(ItemKey::MusicBrainzReleaseGroupId),
        musicbrainz_artist_id: can(ItemKey::MusicBrainzArtistId),
    };
    let metadata =
        fs::metadata(path).map_err(|error| crate::SourceMetadataError::Write(error.to_string()))?;
    let modified = metadata
        .modified()
        .ok()
        .and_then(|value| value.duration_since(std::time::UNIX_EPOCH).ok())
        .map_or(0, |value| value.as_nanos());
    Ok(crate::TrackMetadata {
        track_key,
        writable,
        source_search: false,
        revision: Some(format!("{}:{modified}", metadata.len())),
        source_values: values.clone(),
        values,
        rufin_filled: crate::TrackMetadataWritable::default(),
    })
}

pub(super) fn read_album_metadata_values(
    path: &Path,
    source_format: Option<&str>,
) -> Result<crate::AlbumMetadataValues, crate::SourceMetadataError> {
    let writer = source_format
        .and_then(lofty_metadata::MetadataWriter::for_source_format)
        .or_else(|| lofty_metadata::MetadataWriter::for_path(path))
        .ok_or(crate::SourceMetadataError::Unavailable)?;
    let tagged = lofty_metadata::read_lofty_for_edit(path, writer.file_type())
        .map_err(|error| crate::SourceMetadataError::Write(error.to_string()))?
        .ok_or(crate::SourceMetadataError::Unavailable)?;
    let tag = tagged.primary_tag().or_else(|| tagged.first_tag());
    let text = |key| {
        tag.and_then(|tag| tag.get_string(key))
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(ToString::to_string)
    };
    Ok(crate::AlbumMetadataValues {
        title: tag
            .and_then(|tag| tag.album())
            .map(|value| value.trim().to_string())
            .unwrap_or_default(),
        sort_title: text(ItemKey::AlbumTitleSortOrder),
        artist: text(ItemKey::TrackArtist),
        album_artist: text(ItemKey::AlbumArtist),
        year: tag.and_then(|tag| tag.date()).map(|value| value.year),
        genre: tag
            .and_then(|tag| tag.genre())
            .map(|value| value.trim().to_string()),
        comment: tag
            .and_then(|tag| tag.comment())
            .map(|value| value.trim().to_string()),
        locked: None,
        musicbrainz_album_id: text(ItemKey::MusicBrainzReleaseId),
        musicbrainz_release_group_id: text(ItemKey::MusicBrainzReleaseGroupId),
    })
}

pub(super) fn read_artist_metadata_values(
    path: &Path,
    source_format: Option<&str>,
    fallback_name: &str,
) -> Result<crate::ArtistMetadataValues, crate::SourceMetadataError> {
    let writer = source_format
        .and_then(lofty_metadata::MetadataWriter::for_source_format)
        .or_else(|| lofty_metadata::MetadataWriter::for_path(path))
        .ok_or(crate::SourceMetadataError::Unavailable)?;
    let tagged = lofty_metadata::read_lofty_for_edit(path, writer.file_type())
        .map_err(|error| crate::SourceMetadataError::Write(error.to_string()))?
        .ok_or(crate::SourceMetadataError::Unavailable)?;
    let tag = tagged.primary_tag().or_else(|| tagged.first_tag());
    let text = |key| {
        tag.and_then(|tag| tag.get_string(key))
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(ToString::to_string)
    };
    Ok(crate::ArtistMetadataValues {
        name: fallback_name.to_string(),
        sort_name: text(ItemKey::TrackArtistSortOrder),
        genre: tag
            .and_then(|tag| tag.genre())
            .map(|value| value.trim().to_string()),
        comment: tag
            .and_then(|tag| tag.comment())
            .map(|value| value.trim().to_string()),
        locked: None,
        musicbrainz_artist_id: text(ItemKey::MusicBrainzArtistId),
    })
}
