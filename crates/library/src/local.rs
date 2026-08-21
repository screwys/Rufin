//! Durable observations used by Local's exact no-op and dependency decisions.
//!
//! Sources owns filesystem walking and parsing. Library persists only the
//! accepted observation and applies canonical item changes; one unreadable
//! file or invalid CUE is not a source-level failure.

use std::collections::{BTreeMap, BTreeSet, HashSet, VecDeque};
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::{
    AcceptedHomeChange, AcceptedLibraryChange, Album, AlbumId, Artist, ArtistId, Genre, GenreId,
    Library, LibraryResult, Track, TrackId,
    loaded::{ItemReplacement, LocalArtworkItemId},
};

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub enum LocalFileKind {
    Media,
    Cue,
    Image,
    Directory,
}

impl LocalFileKind {
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::Media => "media",
            Self::Cue => "cue",
            Self::Image => "image",
            Self::Directory => "directory",
        }
    }

    pub(crate) fn from_stored(value: &str) -> Option<Self> {
        match value {
            "media" => Some(Self::Media),
            "cue" => Some(Self::Cue),
            "image" => Some(Self::Image),
            "directory" => Some(Self::Directory),
            _ => None,
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub enum LocalFileState {
    Accepted,
    Rejected,
    Unreadable,
    Observed,
}

impl LocalFileState {
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::Accepted => "accepted",
            Self::Rejected => "rejected",
            Self::Unreadable => "unreadable",
            Self::Observed => "observed",
        }
    }

    pub(crate) fn from_stored(value: &str) -> Option<Self> {
        match value {
            "accepted" => Some(Self::Accepted),
            "rejected" => Some(Self::Rejected),
            "unreadable" => Some(Self::Unreadable),
            "observed" => Some(Self::Observed),
            _ => None,
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct LocalFile {
    pub path: String,
    pub root: String,
    pub relative_path: String,
    pub kind: LocalFileKind,
    pub size_bytes: Option<u64>,
    pub mtime_ns: i64,
    pub device_id: Option<u64>,
    pub inode: Option<u64>,
    pub parse_version: Option<u32>,
    pub state: LocalFileState,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub dependencies: Vec<String>,
}

impl LocalFile {
    /// Directories containing this file, from its parent through its
    /// configured music root.
    pub fn directories_to_root(&self) -> impl Iterator<Item = &Path> {
        let root = Path::new(&self.root);
        Path::new(&self.path)
            .parent()
            .into_iter()
            .flat_map(move |parent| {
                parent
                    .ancestors()
                    .take_while(move |directory| directory.starts_with(root))
            })
    }
}

/// The complete post-state of one Local filesystem dependency component.
///
/// Sources decides the component from old and new file/CUE/art relationships
/// and parses it. Library accepts the finished canonical values and file
/// observations atomically; it does not interpret filesystem events.
#[derive(Clone, Debug, Default)]
pub struct LocalComponentReplacement {
    pub observed_at: i64,
    pub files: Vec<LocalFile>,
    pub removed_paths: Vec<String>,
    pub albums: Vec<Album>,
    pub tracks: Vec<Track>,
    pub artists: Vec<Artist>,
    pub genres: Vec<Genre>,
    pub removed_album_ids: Vec<AlbumId>,
    pub removed_track_ids: Vec<TrackId>,
    pub removed_artist_ids: Vec<ArtistId>,
    pub removed_genre_ids: Vec<GenreId>,
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub enum LocalFileSeed {
    Path(String),
    DirectoryTree(String),
    ArtworkDirectory(String),
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct LocalFileBaseline {
    pub files: Vec<LocalFile>,
    pub tracked_media_paths: BTreeSet<String>,
    /// Accepted media counts beneath each requested artwork directory.
    pub accepted_media_counts_by_directory: BTreeMap<String, usize>,
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub enum LocalComponentSeed {
    Path(String),
    DirectoryTree(String),
    ArtworkDirectory(String),
    Album(AlbumId),
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct LocalComponentBaseline {
    pub files: Vec<LocalFile>,
    pub albums: Vec<Album>,
    pub tracks: Vec<Track>,
}

impl Library {
    pub fn accept_local_component(
        &self,
        replacement: LocalComponentReplacement,
    ) -> LibraryResult<Option<AcceptedLibraryChange>> {
        let LocalComponentReplacement {
            observed_at,
            files,
            removed_paths,
            albums,
            tracks,
            artists,
            genres,
            removed_album_ids,
            removed_track_ids,
            removed_artist_ids,
            removed_genre_ids,
        } = replacement;
        let replacement = ItemReplacement {
            albums,
            tracks,
            artists,
            genres,
            removed_albums: removed_album_ids,
            removed_tracks: removed_track_ids,
            removed_artists: removed_artist_ids,
            removed_genres: removed_genre_ids,
        };
        if files.is_empty() && removed_paths.is_empty() && replacement.is_empty() {
            return Ok(None);
        }
        let favorite_update = self.local_favorite_update(&replacement)?;
        let stored = self.store.replace_local_component(
            self.source_id().clone(),
            self.library_id(),
            observed_at,
            files,
            removed_paths,
            replacement,
            favorite_update,
        )?;
        let mut accepted = self.replace_local_component(
            stored.files,
            stored.removed_paths,
            stored.replacement,
            stored.imports,
            stored.favorites,
            stored.activity,
        )?;
        accepted.home = AcceptedHomeChange::Rebuild;
        accepted.download_coverage_changed = true;
        Ok(Some(accepted))
    }
}

impl Library {
    /// Returns only accepted filesystem observations needed to decide whether a
    /// Local notification represents a real change.
    ///
    /// This deliberately does not clone Tracks, Albums, or relationship facts.
    /// The logical component is resolved only after Local has compared these
    /// observations with the filesystem.
    pub fn local_file_baseline(&self, seeds: &[LocalFileSeed]) -> LibraryResult<LocalFileBaseline> {
        let state = self.read_state()?;
        let mut pending = VecDeque::<(String, bool)>::new();
        let mut artwork_directories = BTreeSet::new();
        for seed in seeds {
            match seed {
                LocalFileSeed::Path(path) => {
                    let tree = state
                        .local_files
                        .get(path)
                        .is_some_and(|file| file.kind == LocalFileKind::Directory);
                    pending.push_back((path.clone(), tree));
                }
                LocalFileSeed::DirectoryTree(path) => {
                    if state.local_files.contains_key(path)
                        || state.directory_children.contains_key(path)
                    {
                        pending.push_back((path.clone(), true));
                    } else {
                        let root = std::path::Path::new(path);
                        pending.extend(
                            state
                                .local_files
                                .keys()
                                .filter(|candidate| {
                                    std::path::Path::new(candidate).starts_with(root)
                                })
                                .cloned()
                                .map(|path| (path, false)),
                        );
                    }
                }
                LocalFileSeed::ArtworkDirectory(path) => {
                    artwork_directories.insert(path.clone());
                    pending.extend(
                        state
                            .directory_images
                            .get(path)
                            .into_iter()
                            .flatten()
                            .cloned()
                            .map(|path| (path, false)),
                    );
                }
            }
        }

        let mut visited = HashSet::new();
        let mut expanded = HashSet::new();
        let mut files = Vec::new();
        while let Some((path, tree)) = pending.pop_front() {
            if tree && expanded.insert(path.clone()) {
                pending.extend(
                    state
                        .directory_children
                        .get(&path)
                        .into_iter()
                        .flatten()
                        .cloned()
                        .map(|path| (path, true)),
                );
            }
            if !visited.insert(path.clone()) {
                continue;
            }
            if let Some(file) = state.local_files.get(&path) {
                files.push(file.clone());
                if file.kind == LocalFileKind::Cue {
                    pending.extend(file.dependencies.iter().cloned().map(|path| (path, false)));
                }
            }
            pending.extend(
                state
                    .cue_dependents
                    .get(&path)
                    .into_iter()
                    .flatten()
                    .cloned()
                    .map(|path| (path, false)),
            );
        }
        files.sort_by(|left, right| left.path.cmp(&right.path));
        let tracked_media_paths = files
            .iter()
            .filter(|file| {
                file.kind == LocalFileKind::Media && state.path_tracks.contains_key(&file.path)
            })
            .map(|file| file.path.clone())
            .collect();
        Ok(LocalFileBaseline {
            files,
            tracked_media_paths,
            accepted_media_counts_by_directory: artwork_directories
                .into_iter()
                .map(|directory| {
                    let count = state
                        .directory_media_counts
                        .get(&directory)
                        .copied()
                        .unwrap_or_default();
                    (directory, count)
                })
                .collect(),
        })
    }

    pub fn local_component_baseline(
        &self,
        seeds: &[LocalComponentSeed],
    ) -> LibraryResult<LocalComponentBaseline> {
        let state = self.read_state()?;
        let mut pending_paths = VecDeque::<(String, bool)>::new();
        let mut pending_tracks = VecDeque::<TrackId>::new();
        let mut pending_albums = VecDeque::<AlbumId>::new();
        for seed in seeds {
            match seed {
                LocalComponentSeed::Path(path) => {
                    let tree = state
                        .local_files
                        .get(path)
                        .is_some_and(|file| file.kind == LocalFileKind::Directory);
                    pending_paths.push_back((path.clone(), tree));
                }
                LocalComponentSeed::DirectoryTree(path) => {
                    if state.local_files.contains_key(path)
                        || state.directory_children.contains_key(path)
                    {
                        pending_paths.push_back((path.clone(), true));
                    } else {
                        let root = std::path::Path::new(path);
                        pending_paths.extend(
                            state
                                .local_files
                                .keys()
                                .filter(|candidate| {
                                    std::path::Path::new(candidate).starts_with(root)
                                })
                                .cloned()
                                .map(|path| (path, false)),
                        );
                    }
                }
                LocalComponentSeed::ArtworkDirectory(path) => {
                    pending_paths.push_back((path.clone(), true));
                }
                LocalComponentSeed::Album(id) => {
                    pending_albums.push_back(id.clone());
                }
            }
        }

        let mut visited_paths = HashSet::new();
        let mut expanded_directories = HashSet::new();
        let mut file_paths = HashSet::new();
        let mut track_ids = HashSet::new();
        let mut album_ids = HashSet::new();

        while !(pending_paths.is_empty() && pending_tracks.is_empty() && pending_albums.is_empty())
        {
            if let Some((path, tree)) = pending_paths.pop_front() {
                let file = state.local_files.get(&path);
                if tree && expanded_directories.insert(path.clone()) {
                    pending_paths.extend(
                        state
                            .directory_children
                            .get(&path)
                            .into_iter()
                            .flatten()
                            .cloned()
                            .map(|child| (child, true)),
                    );
                }
                if !visited_paths.insert(path.clone()) {
                    continue;
                }
                if let Some(file) = file {
                    file_paths.insert(path.clone());
                    if file.kind == LocalFileKind::Cue {
                        pending_paths.extend(
                            file.dependencies
                                .iter()
                                .cloned()
                                .map(|dependency| (dependency, false)),
                        );
                    }
                }
                pending_paths.extend(
                    state
                        .cue_dependents
                        .get(&path)
                        .into_iter()
                        .flatten()
                        .cloned()
                        .map(|cue| (cue, false)),
                );
                pending_tracks.extend(
                    state
                        .path_tracks
                        .get(&path)
                        .into_iter()
                        .flatten()
                        .filter_map(|slot| state.tracks.get_slot(*slot))
                        .map(|track| track.id.clone()),
                );
                for item in state.artwork_items.get(&path).into_iter().flatten() {
                    match item {
                        LocalArtworkItemId::Album(slot) => pending_albums
                            .extend(state.albums.get_slot(*slot).map(|album| album.id.clone())),
                        LocalArtworkItemId::Track(slot) => pending_tracks
                            .extend(state.tracks.get_slot(*slot).map(|track| track.id.clone())),
                        LocalArtworkItemId::Artist(_) => {}
                    }
                }
                continue;
            }

            if let Some(id) = pending_tracks.pop_front() {
                if !track_ids.insert(id.clone()) {
                    continue;
                }
                let Some(track) = state.tracks.get(&id) else {
                    continue;
                };
                pending_paths.extend(
                    [
                        track.source_path.as_deref(),
                        track.cue.as_ref().map(|cue| cue.cue_path.as_str()),
                        track.local_artwork.as_ref().map(|artwork| artwork.path()),
                    ]
                    .into_iter()
                    .flatten()
                    .map(|path| (path.to_string(), false)),
                );
                pending_albums.extend(track.album_id.iter().cloned());
                continue;
            }

            if let Some(id) = pending_albums.pop_front() {
                if !album_ids.insert(id.clone()) {
                    continue;
                }
                let Some(album) = state.albums.get(&id) else {
                    continue;
                };
                if let Some(artwork) = &album.local_artwork {
                    pending_paths.push_back((artwork.path().to_string(), false));
                }
                pending_tracks.extend(
                    album
                        .tracks
                        .iter()
                        .filter_map(|slot| state.tracks.get_slot(*slot))
                        .map(|track| track.id.clone()),
                );
            }
        }

        let mut baseline = LocalComponentBaseline {
            files: file_paths
                .into_iter()
                .filter_map(|path| state.local_files.get(&path).cloned())
                .collect(),
            albums: album_ids
                .into_iter()
                .filter_map(|id| state.albums.get(&id))
                .filter(|album| album.source_provided)
                .map(|album| album.album.as_ref().clone())
                .collect(),
            tracks: track_ids
                .into_iter()
                .filter_map(|id| state.tracks.get(&id).cloned())
                .collect(),
        };
        baseline
            .files
            .sort_by(|left, right| left.path.cmp(&right.path));
        baseline
            .albums
            .sort_by(|left, right| left.id.cmp(&right.id));
        baseline
            .tracks
            .sort_by(|left, right| left.id.cmp(&right.id));
        Ok(baseline)
    }
}
