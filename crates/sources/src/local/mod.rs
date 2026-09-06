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

pub fn read_embedded_lyrics(path: &Path) -> Result<Option<String>, crate::SourceMetadataError> {
    metadata::read_embedded_lyrics(path)
}

pub fn read_local_image(reference: &crate::LocalImageRef) -> SourceResult<ImageBytes> {
    let reference = match reference {
        crate::LocalImageRef::File { path, .. } => {
            artwork::ArtworkReference::File(PathBuf::from(path))
        }
        crate::LocalImageRef::Embedded {
            path,
            picture_index,
            ..
        } => artwork::ArtworkReference::Embedded {
            path: PathBuf::from(path),
            picture_index: *picture_index,
        },
    };
    artwork::read_image(&reference)
}

pub(super) fn write_embedded_lyrics(
    path: &Path,
    lyrics: &str,
) -> Result<(), crate::SourceMetadataError> {
    metadata::write_embedded_lyrics(path, lyrics)
}

pub(super) fn embedded_lyrics_writable(path: &Path) -> bool {
    lofty_metadata::MetadataWriter::for_path(path).is_some_and(|writer| {
        !matches!(
            writer.file_type(),
            lofty::file::FileType::Wav | lofty::file::FileType::Aiff
        ) && writer.lyrics_target().is_some()
    })
}

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

    pub(crate) async fn import_playlist_files(
        &self,
        database: &library::Database,
        source_id: &str,
        playlist: library::PlaylistKey,
    ) -> SourceResult<library::ScanOutcome> {
        database
            .import_local_playlist_paths(source_id, playlist)
            .await?;
        let mut scan = library::Scan::begin_items(database, source_id).await?;
        scan::stage_imported_paths(database, &mut scan).await?;
        scan::stage_artwork(database, &mut scan, &|| false).await?;
        Ok(scan.finish().await?)
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

pub(crate) fn connect(
    source_id: crate::SourceId,
    input: LocalFolderHostInput,
) -> SourceResult<ConnectedSource> {
    let source = LocalSource::from_roots(input.roots)?;
    let configuration = crate::config::encode_provider_payload(
        source_id,
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
    Ok(normalized)
}

pub(super) fn metadata_file_available(path: &Path, source_format: Option<&str>) -> bool {
    source_format
        .and_then(lofty_metadata::MetadataWriter::for_source_format)
        .or_else(|| lofty_metadata::MetadataWriter::for_path(path))
        .is_some()
}

pub(super) fn read_track_metadata(
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
