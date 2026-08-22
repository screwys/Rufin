//! Bounded source-library candidate acceptance.
//!
//! Concrete sources acquire facts; Rufin feeds those facts into this explicit
//! candidate. Library alone persists, compares, and accepts them. Dropping an
//! unfinished candidate makes it invisible and schedules bounded cleanup.

use std::sync::Arc;

use crate::loaded::ItemReplacement;
use crate::{
    Album, AlbumId, Artist, ArtistId, Genre, GenreId, HomeFacts, Libraries, Library, LibraryError,
    LibraryResult, LocalFile, MoodId, MusicFolder, PlaylistId, PlaylistSnapshot, SmartPlaylistId,
    SourceId, Track, TrackId,
};
pub const STORE_ROW_BATCH_LIMIT: usize = 500;
pub const STORE_BYTE_BATCH_LIMIT: usize = 8 * 1024 * 1024;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProviderFreshness {
    pub version: u32,
    pub marker: Vec<u8>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CandidateHeader {
    pub source_id: SourceId,
    pub input_digest: [u8; 32],
}

#[derive(Clone, Debug)]
pub enum CandidateBatch {
    Albums(Vec<Album>),
    Tracks(Vec<Track>),
    Artists(Vec<Artist>),
    Genres(Vec<Genre>),
    MusicFolders(Vec<MusicFolder>),
    Playlists(Vec<PlaylistSnapshot>),
    LocalFiles(Vec<LocalFile>),
}

impl CandidateBatch {
    pub fn len(&self) -> usize {
        match self {
            Self::Albums(values) => values.len(),
            Self::Tracks(values) => values.len(),
            Self::Artists(values) => values.len(),
            Self::Genres(values) => values.len(),
            Self::MusicFolders(values) => values.len(),
            Self::Playlists(values) => values.len(),
            Self::LocalFiles(values) => values.len(),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

#[derive(Clone, Debug)]
pub struct CandidateFinish {
    pub freshness: Option<ProviderFreshness>,
    pub home: HomeFacts,
    pub accepted_at: i64,
}

#[derive(Clone, Debug)]
pub struct CandidateCommit {
    pub change: CandidateChange,
    pub library: Arc<Library>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CandidateChange {
    Library,
    Home,
    None,
}

/// A complete candidate whose canonical facts can be inspected and prepared
/// without making them durable or visible.
///
/// Rufin prepares artwork and current-media replacements from `library`, then
/// consumes this value with [`accept`](Self::accept). Dropping it keeps the
/// previously accepted source library and schedules the invisible rows for
/// bounded cleanup.
pub struct PreparedSourceCandidate {
    libraries: Libraries,
    candidate_library_id: i64,
    prepared: Option<crate::store::PreparedStoreCandidate>,
    library: Arc<Library>,
    finished: bool,
}

/// One source-authoritative exact update.
///
/// Concrete sources resolve their private IDs and decide when a complete
/// refresh is required before constructing this value. Items and affected
/// playlist readbacks are accepted together so no partial source state is
/// visible or durable.
#[derive(Clone, Debug, Default)]
pub struct SourceLibraryUpdate {
    pub albums: Vec<Album>,
    pub tracks: Vec<Track>,
    pub artists: Vec<Artist>,
    pub removed_tracks: Vec<TrackId>,
    pub playlists: Vec<PlaylistSnapshot>,
    pub removed_playlists: Vec<PlaylistId>,
}

#[derive(Clone, Debug)]
pub struct AcceptedTrackReplacement {
    pub id: TrackId,
    pub track: Option<Track>,
    pub activity_only: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FavoriteAcknowledgement {
    pub item: crate::FavoriteItemId,
    pub favorite: bool,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub enum AcceptedHomeChange {
    #[default]
    Keep,
    Rebuild,
    Favorite(crate::FavoriteItemId),
    Played(TrackId),
}

#[derive(Clone, Debug, Default)]
pub struct AcceptedLibraryChange {
    pub tracks: Vec<AcceptedTrackReplacement>,
    pub albums: Vec<AlbumId>,
    pub artists: Vec<ArtistId>,
    pub artist_releases: Vec<ArtistId>,
    pub genres: Vec<GenreId>,
    pub moods: Vec<MoodId>,
    pub playlists: Vec<PlaylistId>,
    pub smart_playlists: Vec<SmartPlaylistId>,
    pub local_folders_changed: bool,
    pub history_changed: bool,
    pub favorite: Option<FavoriteAcknowledgement>,
    pub home: AcceptedHomeChange,
    pub download_coverage_changed: bool,
    pub album_release_candidates_changed: bool,
}

pub struct SourceCandidate {
    libraries: Libraries,
    source_id: SourceId,
    library_id: i64,
    write_failed: bool,
    finished: bool,
}

impl Library {
    pub fn accept_source_update(
        &self,
        update: SourceLibraryUpdate,
    ) -> LibraryResult<Option<AcceptedLibraryChange>> {
        if update.albums.is_empty()
            && update.tracks.is_empty()
            && update.artists.is_empty()
            && update.removed_tracks.is_empty()
            && update.playlists.is_empty()
            && update.removed_playlists.is_empty()
        {
            return Ok(None);
        }
        let source_items_changed = !update.albums.is_empty()
            || !update.tracks.is_empty()
            || !update.removed_tracks.is_empty();
        let SourceLibraryUpdate {
            albums,
            tracks,
            artists,
            removed_tracks,
            mut playlists,
            mut removed_playlists,
        } = update;
        let mut replacement = ItemReplacement {
            albums,
            tracks,
            artists,
            removed_tracks,
            ..ItemReplacement::default()
        };
        self.keep_changed_source_update(&mut replacement, &mut playlists, &mut removed_playlists)?;
        if replacement.is_empty() && playlists.is_empty() && removed_playlists.is_empty() {
            return Ok(None);
        }
        let stored = self.store.replace_source_update(
            self.source_id().clone(),
            self.library_id(),
            replacement,
            playlists,
            removed_playlists,
        )?;
        let mut accepted = self.replace_source_update(
            stored.replacement,
            stored.playlists,
            stored.removed_playlists,
        )?;
        accepted.home = if source_items_changed {
            AcceptedHomeChange::Rebuild
        } else {
            AcceptedHomeChange::Keep
        };
        accepted.download_coverage_changed = true;
        accepted.album_release_candidates_changed = source_items_changed;
        Ok(Some(accepted))
    }
}

impl SourceCandidate {
    pub(crate) fn new(libraries: Libraries, header: CandidateHeader, library_id: i64) -> Self {
        let source_id = header.source_id;
        Self {
            libraries,
            source_id,
            library_id,
            finished: false,
            write_failed: false,
        }
    }

    pub fn source_id(&self) -> &SourceId {
        &self.source_id
    }

    pub fn write(&mut self, batch: CandidateBatch) -> LibraryResult<()> {
        if self.write_failed {
            return Err(LibraryError::CandidateWriteFailed);
        }
        if batch.is_empty() {
            return Ok(());
        }
        match self.libraries.write_candidate(self.library_id, batch) {
            Ok(()) => Ok(()),
            Err(error) => {
                self.write_failed = true;
                self.libraries.schedule_candidate_cleanup(self.library_id);
                Err(error)
            }
        }
    }

    pub fn finish(
        mut self,
        finish: CandidateFinish,
        current: Option<&Arc<Library>>,
    ) -> LibraryResult<PreparedSourceCandidate> {
        if self.write_failed {
            return Err(LibraryError::CandidateWriteFailed);
        }
        let preparation = self
            .libraries
            .store
            .prepare_candidate(self.library_id, finish)?;
        let library = if let Some(input) = preparation.input {
            Library::build(
                input,
                self.libraries.store.clone(),
                Arc::clone(&self.libraries.home_sessions),
            )?
        } else if let Some(library) = current.filter(|library| {
            library.source_id() == &self.source_id
                && library.store.same_lane(&self.libraries.store)
                && Some(library.library_id()) == preparation.prepared.current_library_id()
        }) {
            Arc::clone(library)
        } else {
            self.libraries
                .load_source(&self.source_id)?
                .filter(|library| {
                    Some(library.library_id()) == preparation.prepared.current_library_id()
                })
                .ok_or_else(|| {
                    LibraryError::Persistence(
                        "the accepted source library could not be prepared".to_string(),
                    )
                })?
        };
        self.finished = true;
        Ok(PreparedSourceCandidate {
            libraries: self.libraries.clone(),
            candidate_library_id: self.library_id,
            prepared: Some(preparation.prepared),
            library,
            finished: false,
        })
    }
}

impl Drop for SourceCandidate {
    fn drop(&mut self) {
        if !self.finished {
            self.libraries.schedule_candidate_cleanup(self.library_id);
        }
    }
}

impl PreparedSourceCandidate {
    pub const fn change(&self) -> CandidateChange {
        let Some(prepared) = &self.prepared else {
            panic!("an unconsumed source candidate retains its Store preparation");
        };
        prepared.change()
    }

    pub fn library(&self) -> &Arc<Library> {
        &self.library
    }

    pub fn accept(mut self) -> LibraryResult<CandidateCommit> {
        let prepared = self.prepared.take().ok_or_else(|| {
            LibraryError::Persistence("the source candidate was already consumed".to_string())
        })?;
        let change = prepared.change();
        let commit = self.libraries.store.accept_candidate(prepared)?;
        // Store acceptance is now durable. A later in-process publication
        // error must not make Drop delete the accepted source library.
        self.finished = true;
        let library = match change {
            CandidateChange::None => {
                self.library.replace_provider_freshness(commit.freshness)?;
                Arc::clone(&self.library)
            }
            CandidateChange::Home => {
                self.library.replace_home_facts(commit.home)?;
                self.library.replace_provider_freshness(commit.freshness)?;
                Arc::clone(&self.library)
            }
            CandidateChange::Library => {
                self.library
                    .replace_activity_snapshot(commit.activity, commit.recent_plays)?;
                Arc::clone(&self.library)
            }
        };
        Ok(CandidateCommit { change, library })
    }
}

impl Drop for PreparedSourceCandidate {
    fn drop(&mut self) {
        if !self.finished {
            self.libraries
                .schedule_candidate_cleanup(self.candidate_library_id);
        }
    }
}
