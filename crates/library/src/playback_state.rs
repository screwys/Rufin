//! Durable source-scoped Playback state.
//!
//! Queue structure is replaced atomically. Current occurrence and progress
//! updates are narrow revision-guarded writes that never serialize the queue
//! again. App-wide playback preferences do not belong to a source checkpoint.

use std::collections::HashSet;

use serde::{Deserialize, Serialize};

use crate::{
    AlbumId, ArtistId, CueSegment, ImageRef, Libraries, LibraryError, LibraryResult,
    LocalArtworkRef, SourceId, TrackId,
};

#[derive(Clone, Debug, Deserialize, Eq, Hash, PartialEq, Serialize)]
pub struct PlaybackOccurrenceId(String);

impl PlaybackOccurrenceId {
    pub fn new(value: impl Into<String>) -> Self {
        let value = value.into();
        assert!(!value.is_empty(), "Playback occurrence ID cannot be empty");
        Self(value)
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub enum PlaybackProvenance {
    Context {
        context_id: String,
        source_rank: usize,
    },
    Manual,
    Random,
    Radio,
    AutoDj,
    Legacy,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct PlaybackOccurrence {
    pub id: PlaybackOccurrenceId,
    pub track_id: TrackId,
    pub provenance: PlaybackProvenance,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct PlaybackFallbackTrack {
    pub id: TrackId,
    pub album_id: Option<AlbumId>,
    pub primary_artist_id: Option<ArtistId>,
    pub title: String,
    pub artist: String,
    pub album: String,
    pub year: u16,
    pub duration_seconds: u32,
    pub favorite: bool,
    pub track_number: u16,
    pub disc_number: u16,
    pub image_ref: Option<ImageRef>,
    pub local_artwork: Option<LocalArtworkRef>,
    pub musicbrainz_recording_id: Option<String>,
    pub source_format: Option<String>,
    pub source_path: Option<String>,
    pub cue: Option<CueSegment>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct PlaybackQueueSnapshot {
    pub occurrences: Vec<PlaybackOccurrence>,
    pub fallback_tracks: Vec<PlaybackFallbackTrack>,
    pub traversal: Vec<PlaybackOccurrenceId>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub(crate) struct PlaybackQueueRowsSnapshot {
    pub occurrences: Vec<PlaybackOccurrence>,
    pub fallback_tracks: Vec<PlaybackFallbackTrack>,
}

#[derive(Serialize)]
pub(crate) struct PlaybackQueueRows<'a> {
    occurrences: &'a [PlaybackOccurrence],
    fallback_tracks: &'a [PlaybackFallbackTrack],
}

impl PlaybackQueueSnapshot {
    pub(crate) fn rows(&self) -> PlaybackQueueRows<'_> {
        PlaybackQueueRows {
            occurrences: &self.occurrences,
            fallback_tracks: &self.fallback_tracks,
        }
    }
}

impl PlaybackQueueRowsSnapshot {
    pub(crate) fn with_traversal(
        self,
        traversal: Vec<PlaybackOccurrenceId>,
    ) -> PlaybackQueueSnapshot {
        PlaybackQueueSnapshot {
            occurrences: self.occurrences,
            fallback_tracks: self.fallback_tracks,
            traversal,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PlaybackState {
    pub selected: Option<PlaybackOccurrenceId>,
    pub progress_millis: u64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PlaybackCheckpoint {
    pub source_id: SourceId,
    pub revision: u64,
    pub queue: PlaybackQueueSnapshot,
    pub state: PlaybackState,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PlaybackStateUpdate {
    pub source_id: SourceId,
    pub revision: u64,
    pub selected: Option<PlaybackOccurrenceId>,
    pub progress_millis: u64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PlaybackProgressUpdate {
    pub source_id: SourceId,
    pub revision: u64,
    pub occurrence: PlaybackOccurrenceId,
    pub progress_millis: u64,
}

/// A queue revision whose rows are unchanged and whose traversal changed.
///
/// The Store applies this only while the durable queue is still at
/// `expected_revision`, so a delayed reshuffle cannot overwrite a newer queue.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PlaybackTraversalUpdate {
    pub source_id: SourceId,
    pub expected_revision: u64,
    pub revision: u64,
    pub traversal: Vec<PlaybackOccurrenceId>,
    pub state: PlaybackState,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PlaybackWriteOutcome {
    Applied,
    Stale,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum PlaybackLoad {
    Missing,
    Ready(PlaybackCheckpoint),
    DiscardedCorrupt,
}

impl Libraries {
    pub fn load_playback(&self, source_id: &SourceId) -> LibraryResult<PlaybackLoad> {
        Ok(self.store.load_playback(source_id.clone())?)
    }

    pub fn replace_playback(
        &self,
        checkpoint: PlaybackCheckpoint,
    ) -> LibraryResult<PlaybackWriteOutcome> {
        validate_checkpoint(&checkpoint).map_err(LibraryError::Persistence)?;
        Ok(self.store.replace_playback(checkpoint)?)
    }

    pub fn update_playback_state(
        &self,
        update: PlaybackStateUpdate,
    ) -> LibraryResult<PlaybackWriteOutcome> {
        Ok(self.store.update_playback_state(update)?)
    }

    pub fn replace_playback_traversal(
        &self,
        update: PlaybackTraversalUpdate,
    ) -> LibraryResult<PlaybackWriteOutcome> {
        Ok(self.store.replace_playback_traversal(update)?)
    }

    pub fn update_playback_progress(
        &self,
        update: PlaybackProgressUpdate,
    ) -> LibraryResult<PlaybackWriteOutcome> {
        Ok(self.store.update_playback_progress(update)?)
    }

    pub fn remove_playback(&self, source_id: &SourceId) -> LibraryResult<bool> {
        Ok(self.store.remove_playback(source_id.clone())?)
    }
}

pub(crate) fn validate_checkpoint(checkpoint: &PlaybackCheckpoint) -> Result<(), String> {
    validate_queue_parts(
        &checkpoint.queue.occurrences,
        &checkpoint.queue.fallback_tracks,
        &checkpoint.queue.traversal,
        checkpoint.state.selected.as_ref(),
    )
}

pub(crate) fn validate_queue_parts(
    queue: &[PlaybackOccurrence],
    fallback: &[PlaybackFallbackTrack],
    traversal: &[PlaybackOccurrenceId],
    selected: Option<&PlaybackOccurrenceId>,
) -> Result<(), String> {
    let occurrences = queue.iter().map(|entry| &entry.id).collect::<HashSet<_>>();
    if occurrences.len() != queue.len() {
        return Err("Playback occurrence IDs must be unique".to_string());
    }

    let queued_tracks = queue
        .iter()
        .map(|entry| &entry.track_id)
        .collect::<HashSet<_>>();
    let fallback_tracks = fallback
        .iter()
        .map(|track| &track.id)
        .collect::<HashSet<_>>();
    if fallback_tracks.len() != fallback.len() || fallback_tracks != queued_tracks {
        return Err("Playback fallback Tracks must match the distinct queued Tracks".to_string());
    }

    if !traversal.is_empty() {
        let traversal_ids = traversal.iter().collect::<HashSet<_>>();
        if traversal_ids.len() != traversal.len() || traversal_ids != occurrences {
            return Err("Playback traversal must contain every occurrence once".to_string());
        }
    }

    if selected.is_some_and(|selected| !occurrences.contains(selected)) {
        return Err("selected Playback occurrence is not queued".to_string());
    }
    Ok(())
}
