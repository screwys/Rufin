use library::{
    ArtistCredit, Library, PlaybackCheckpoint, PlaybackFallbackTrack, PlaybackOccurrence,
    PlaybackOccurrenceId, PlaybackProvenance, PlaybackQueueSnapshot, PlaybackState,
    PlaybackTraversalUpdate, SourceId, Track, TrackData, TrackId, TrackRelations,
};
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use thiserror::Error;

use crate::{
    OccurrenceId, Provenance, RepeatMode, RestoredSequence, Sequence, SequenceEntry, SequenceError,
};

#[derive(Debug, Error)]
pub enum CheckpointError {
    #[error("playback checkpoint sequence is invalid: {0}")]
    Sequence(#[from] SequenceError),
    #[error("playback checkpoint has no fallback for Track {0}")]
    MissingFallback(TrackId),
    #[error("playback checkpoint could not read the Library: {0}")]
    Loaded(String),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct CheckpointChange {
    pub expected_revision: u64,
    pub rows_changed: bool,
}

/// An immutable durable queue revision captured without copying queue rows.
///
/// Playback publishes this value to its persistence consumer. The blocking
/// Store worker materializes it only after newer revisions have been
/// coalesced, keeping queue serialization and fallback construction off the
/// playback actor and output threads.
#[derive(Clone, Debug)]
pub struct PlaybackCheckpointRevision {
    source_id: SourceId,
    expected_revision: u64,
    revision: u64,
    rows_changed: bool,
    rows: Arc<Vec<SequenceEntry>>,
    traversal: Arc<Vec<usize>>,
    shuffle_enabled: bool,
    selected_index: Option<usize>,
    progress_millis: u64,
}

#[derive(Debug)]
pub enum PlaybackCheckpointMaterialization {
    Full(PlaybackCheckpoint),
    Traversal(PlaybackTraversalUpdate),
}

impl PlaybackCheckpointRevision {
    pub(crate) fn capture(sequence: &Sequence, change: CheckpointChange) -> Self {
        Self {
            source_id: sequence.source_id.clone(),
            expected_revision: change.expected_revision,
            revision: sequence.revision,
            rows_changed: change.rows_changed,
            rows: Arc::clone(&sequence.entries),
            traversal: Arc::clone(&sequence.traversal),
            shuffle_enabled: sequence.shuffle_enabled,
            selected_index: sequence.selected_index,
            progress_millis: sequence.progress_millis,
        }
    }

    pub fn source_id(&self) -> &SourceId {
        &self.source_id
    }

    pub fn revision(&self) -> u64 {
        self.revision
    }

    #[cfg(test)]
    fn shares_rows_with(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.rows, &other.rows)
    }

    /// Keeps only the latest value while retaining the oldest durable base.
    pub fn coalesce(&mut self, mut newer: Self) {
        if self.source_id == newer.source_id && self.revision == newer.expected_revision {
            newer.expected_revision = self.expected_revision;
            newer.rows_changed |= self.rows_changed;
        } else {
            newer.rows_changed = true;
        }
        *self = newer;
    }

    pub fn materialize_checkpoint(self) -> PlaybackCheckpointMaterialization {
        if self.rows_changed || self.revision <= self.expected_revision {
            return PlaybackCheckpointMaterialization::Full(self.materialize_full_checkpoint());
        }
        let traversal = checkpoint_traversal(&self.rows, &self.traversal, self.shuffle_enabled);
        let state = checkpoint_state(&self.rows, self.selected_index, self.progress_millis);
        PlaybackCheckpointMaterialization::Traversal(PlaybackTraversalUpdate {
            source_id: self.source_id,
            expected_revision: self.expected_revision,
            revision: self.revision,
            traversal,
            state,
        })
    }

    pub fn materialize_full_checkpoint(self) -> PlaybackCheckpoint {
        let traversal = checkpoint_traversal(&self.rows, &self.traversal, self.shuffle_enabled);
        let state = checkpoint_state(&self.rows, self.selected_index, self.progress_millis);
        let (occurrences, fallback_tracks) = checkpoint_rows(&self.rows);
        PlaybackCheckpoint {
            source_id: self.source_id,
            revision: self.revision,
            queue: PlaybackQueueSnapshot {
                occurrences,
                fallback_tracks,
                traversal,
            },
            state,
        }
    }
}

fn checkpoint_rows(
    entries: &[SequenceEntry],
) -> (Vec<PlaybackOccurrence>, Vec<PlaybackFallbackTrack>) {
    let mut seen = HashSet::<&TrackId>::new();
    let fallback_tracks = entries
        .iter()
        .filter(|entry| seen.insert(&entry.track.id))
        .map(|entry| fallback_track(&entry.track))
        .collect();
    let occurrences = entries
        .iter()
        .map(|entry| PlaybackOccurrence {
            id: PlaybackOccurrenceId::new(entry.occurrence.as_str()),
            track_id: entry.track.id.clone(),
            provenance: playback_provenance(&entry.provenance),
        })
        .collect();
    (occurrences, fallback_tracks)
}

fn checkpoint_traversal(
    entries: &[SequenceEntry],
    traversal: &[usize],
    shuffle_enabled: bool,
) -> Vec<PlaybackOccurrenceId> {
    if shuffle_enabled {
        traversal
            .iter()
            .filter_map(|index| entries.get(*index))
            .map(|entry| PlaybackOccurrenceId::new(entry.occurrence.as_str()))
            .collect()
    } else {
        Vec::new()
    }
}

fn checkpoint_state(
    entries: &[SequenceEntry],
    selected_index: Option<usize>,
    progress_millis: u64,
) -> PlaybackState {
    PlaybackState {
        selected: selected_index
            .and_then(|index| entries.get(index))
            .map(|entry| PlaybackOccurrenceId::new(entry.occurrence.as_str())),
        progress_millis,
    }
}

pub fn restore_checkpoint(
    checkpoint: &PlaybackCheckpoint,
    loaded: Option<&Library>,
    repeat_mode: RepeatMode,
    shuffle_enabled: bool,
    shuffle_seed: u64,
) -> Result<Sequence, CheckpointError> {
    let fallbacks = checkpoint
        .queue
        .fallback_tracks
        .iter()
        .map(|fallback| (&fallback.id, fallback))
        .collect::<HashMap<_, _>>();
    let mut tracks = HashMap::new();
    if let Some(loaded) = loaded.filter(|loaded| loaded.source_id() == &checkpoint.source_id) {
        for track_id in checkpoint
            .queue
            .occurrences
            .iter()
            .map(|occurrence| &occurrence.track_id)
            .collect::<HashSet<_>>()
        {
            if let Some(track) = loaded
                .track(track_id)
                .map_err(|error| CheckpointError::Loaded(error.to_string()))?
            {
                tracks.insert(track_id.clone(), track);
            }
        }
    }
    let entries = checkpoint
        .queue
        .occurrences
        .iter()
        .map(|occurrence| {
            if !tracks.contains_key(&occurrence.track_id) {
                let fallback = fallbacks
                    .get(&occurrence.track_id)
                    .ok_or_else(|| CheckpointError::MissingFallback(occurrence.track_id.clone()))?;
                tracks.insert(occurrence.track_id.clone(), track_from_fallback(fallback));
            }
            let track = tracks
                .get(&occurrence.track_id)
                .expect("resolved current or fallback Track")
                .clone();
            Ok(SequenceEntry {
                occurrence: OccurrenceId::new(occurrence.id.as_str()),
                track,
                provenance: sequence_provenance(&occurrence.provenance),
            })
        })
        .collect::<Result<Vec<_>, CheckpointError>>()?;
    let stored_traversal = checkpoint
        .queue
        .traversal
        .iter()
        .map(|occurrence| OccurrenceId::new(occurrence.as_str()))
        .collect::<Vec<_>>();
    let restore_stored_traversal = shuffle_enabled && !stored_traversal.is_empty();
    let mut sequence = Sequence::restore(RestoredSequence {
        source_id: checkpoint.source_id.clone(),
        entries,
        selected: checkpoint
            .state
            .selected
            .as_ref()
            .map(|occurrence| OccurrenceId::new(occurrence.as_str())),
        repeat_mode,
        shuffle_enabled: restore_stored_traversal,
        traversal: stored_traversal,
        revision: checkpoint.revision,
        progress_millis: checkpoint.state.progress_millis,
    })?;
    if shuffle_enabled && !restore_stored_traversal {
        let revision = sequence.revision;
        sequence.set_shuffle_seed(true, shuffle_seed);
        sequence.revision = revision;
    }
    Ok(sequence)
}

fn fallback_track(track: &Track) -> PlaybackFallbackTrack {
    let album_artwork = track.album_artwork_facts();
    PlaybackFallbackTrack {
        id: track.id.clone(),
        album_id: track.album_id.clone(),
        primary_artist_id: track.primary_artist_id().cloned(),
        title: track.title.clone(),
        artist: track.artist.clone(),
        album: track.album.clone(),
        year: track.year,
        duration_seconds: track.duration_seconds,
        favorite: track.favorite,
        track_number: track.track_number,
        disc_number: track.disc_number,
        image_ref: album_artwork
            .and_then(|album| album.image_ref.clone())
            .or_else(|| track.image_ref.clone()),
        local_artwork: album_artwork
            .and_then(|album| album.local_artwork.clone())
            .or_else(|| track.local_artwork.clone()),
        musicbrainz_recording_id: track.musicbrainz_recording_id.clone(),
        source_format: track.source_format.clone(),
        source_path: track.source_path.clone(),
        cue: track.cue.clone(),
    }
}

fn track_from_fallback(fallback: &PlaybackFallbackTrack) -> Track {
    let artists = fallback
        .primary_artist_id
        .clone()
        .map(|id| {
            vec![ArtistCredit {
                id,
                name: fallback.artist.clone(),
                musicbrainz_artist_id: None,
            }]
        })
        .unwrap_or_default();
    Track::new(TrackData {
        id: fallback.id.clone(),
        album_id: fallback.album_id.clone(),
        title: fallback.title.clone(),
        artist: fallback.artist.clone(),
        album: fallback.album.clone(),
        album_artwork: None,
        year: fallback.year,
        release_date: None,
        date_added: None,
        last_played: None,
        play_count: None,
        user_rating: None,
        duration_seconds: fallback.duration_seconds,
        favorite: fallback.favorite,
        disc_number: fallback.disc_number,
        track_number: fallback.track_number,
        image_ref: fallback.image_ref.clone(),
        local_artwork: fallback.local_artwork.clone(),
        musicbrainz_recording_id: fallback.musicbrainz_recording_id.clone(),
        musicbrainz_release_track_id: None,
        source_path: fallback.source_path.clone(),
        cue: fallback.cue.clone(),
        source_format: fallback.source_format.clone(),
        comment: None,
        skip_count: None,
        bpm: None,
        relations: TrackRelations {
            artists,
            ..TrackRelations::default()
        },
    })
}

fn playback_provenance(provenance: &Provenance) -> PlaybackProvenance {
    match provenance {
        Provenance::Context {
            context_id,
            source_rank,
        } => PlaybackProvenance::Context {
            context_id: context_id.clone(),
            source_rank: *source_rank,
        },
        Provenance::Manual => PlaybackProvenance::Manual,
        Provenance::Random => PlaybackProvenance::Random,
        Provenance::Radio => PlaybackProvenance::Radio,
        Provenance::AutoDj => PlaybackProvenance::AutoDj,
        Provenance::Legacy => PlaybackProvenance::Legacy,
    }
}

fn sequence_provenance(provenance: &PlaybackProvenance) -> Provenance {
    match provenance {
        PlaybackProvenance::Context {
            context_id,
            source_rank,
        } => Provenance::Context {
            context_id: context_id.clone(),
            source_rank: *source_rank,
        },
        PlaybackProvenance::Manual => Provenance::Manual,
        PlaybackProvenance::Random => Provenance::Random,
        PlaybackProvenance::Radio => Provenance::Radio,
        PlaybackProvenance::AutoDj => Provenance::AutoDj,
        PlaybackProvenance::Legacy => Provenance::Legacy,
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use library::{AlbumArtworkFacts, AlbumId, CueSegment, ImageRef, SourceId, TrackId};

    use super::*;
    use crate::{Batch, BatchItem, Placement};

    #[test]
    fn compact_checkpoint_round_trip_preserves_duplicate_handles_and_cue_playback() {
        let mut sequence = Sequence::new(SourceId::fake(1));
        let track = Track::new(TrackData {
            id: TrackId::fake(1),
            album_id: Some(AlbumId::fake(1)),
            title: "Cue Track".to_string(),
            artist: "Artist".to_string(),
            album: "Album".to_string(),
            album_artwork: Some(Arc::new(AlbumArtworkFacts {
                local_artwork: None,
                image_ref: Some(ImageRef::new("album-art", None)),
                musicbrainz_release_group_id: None,
                musicbrainz_album_id: None,
                artist: "Artist".to_string(),
                title: "Album".to_string(),
            })),
            year: 2026,
            release_date: None,
            date_added: None,
            last_played: None,
            play_count: None,
            user_rating: None,
            duration_seconds: 180,
            favorite: false,
            disc_number: 2,
            track_number: 7,
            image_ref: Some(ImageRef::new("track-art", None)),
            local_artwork: None,
            musicbrainz_recording_id: Some("recording".to_string()),
            musicbrainz_release_track_id: None,
            source_path: Some("/music/disc.flac".to_string()),
            cue: Some(CueSegment {
                cue_path: "/music/disc.cue".to_string(),
                start_millis: 10_000,
                end_millis: 20_000,
            }),
            source_format: Some("FLAC".to_string()),
            comment: None,
            skip_count: None,
            bpm: None,
            relations: TrackRelations::default(),
        });
        sequence
            .apply_batch(
                Batch::new(vec![
                    BatchItem::new(track.clone(), Provenance::Manual),
                    BatchItem::new(track.clone(), Provenance::Radio),
                ]),
                Placement::Replace { anchor_index: 1 },
            )
            .expect("apply duplicate Track");
        sequence.set_progress_millis(12_345);
        assert!(sequence.set_shuffle_seed(true, 17));
        let checkpoint = full_checkpoint(&sequence);
        let stored_traversal = checkpoint.queue.traversal.clone();

        assert_eq!(checkpoint.queue.occurrences.len(), 2);
        assert_eq!(checkpoint.queue.fallback_tracks.len(), 1);
        assert_eq!(
            checkpoint.queue.fallback_tracks[0]
                .image_ref
                .as_ref()
                .map(|image| image.item_id.as_str()),
            Some("album-art")
        );
        let restored = restore_checkpoint(&checkpoint, None, RepeatMode::All, true, 999)
            .expect("restore checkpoint");
        assert_eq!(restored.selected_index(), Some(1));
        assert_eq!(restored.repeat_mode(), RepeatMode::All);
        assert!(restored.shuffle_enabled());
        assert_eq!(restored.progress_millis(), 12_345);
        assert_eq!(
            restored
                .traversal()
                .into_iter()
                .map(|occurrence| PlaybackOccurrenceId::new(occurrence.as_str()))
                .collect::<Vec<_>>(),
            stored_traversal
        );
        assert!(Track::ptr_eq(
            &restored.entries()[0].track,
            &restored.entries()[1].track
        ));
        assert_eq!(
            restored.entries()[0]
                .track
                .cue
                .as_ref()
                .map(|cue| (cue.start_millis, cue.end_millis)),
            Some((10_000, 20_000))
        );
        assert_eq!(
            restored.entries()[0]
                .track
                .musicbrainz_recording_id
                .as_deref(),
            Some("recording")
        );
    }

    #[test]
    fn app_preferences_control_restore_when_no_traversal_was_stored() {
        let mut sequence = Sequence::new(SourceId::fake(1));
        sequence
            .apply_batch(
                Batch::new(vec![item(1), item(2), item(3), item(4)]),
                Placement::Replace { anchor_index: 2 },
            )
            .expect("build queue");
        let checkpoint = full_checkpoint(&sequence);
        assert!(checkpoint.queue.traversal.is_empty());

        let shuffled = restore_checkpoint(&checkpoint, None, RepeatMode::One, true, 81)
            .expect("restore shuffled");
        assert_eq!(shuffled.repeat_mode(), RepeatMode::One);
        assert!(shuffled.shuffle_enabled());
        assert_eq!(
            shuffled
                .traversal()
                .first()
                .map(|occurrence| occurrence.as_str()),
            shuffled.selected().map(|entry| entry.occurrence.as_str())
        );
        assert_eq!(shuffled.revision(), checkpoint.revision);

        let unshuffled = restore_checkpoint(&checkpoint, None, RepeatMode::All, false, 81)
            .expect("restore unshuffled");
        assert_eq!(unshuffled.repeat_mode(), RepeatMode::All);
        assert!(!unshuffled.shuffle_enabled());
        assert_eq!(
            unshuffled
                .traversal()
                .into_iter()
                .map(OccurrenceId::as_str)
                .collect::<Vec<_>>(),
            checkpoint
                .queue
                .occurrences
                .iter()
                .map(|occurrence| occurrence.id.as_str())
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn identical_replacement_reuses_rows_and_materializes_only_new_traversal() {
        let mut sequence = Sequence::new(SourceId::fake(1));
        sequence
            .apply_batch(
                Batch::new((1..=8).map(item).collect()),
                Placement::Replace { anchor_index: 0 },
            )
            .expect("build queue");
        assert!(sequence.set_shuffle_seed(true, 17));
        let previous_revision = sequence.revision();
        let previous = PlaybackCheckpointRevision::capture(
            &sequence,
            CheckpointChange {
                expected_revision: 0,
                rows_changed: true,
            },
        );
        let (_, change) = sequence
            .apply_batch_with_change(
                Batch::new((1..=8).map(item).collect()).with_shuffle_intent(29, true),
                Placement::Replace { anchor_index: 0 },
            )
            .expect("restart identical queue");
        let revision = PlaybackCheckpointRevision::capture(
            &sequence,
            CheckpointChange {
                expected_revision: change.expected_revision,
                rows_changed: change.rows_changed,
            },
        );

        assert!(!change.rows_changed);
        assert!(change.traversal_changed);
        assert_eq!(revision.revision(), previous_revision + 1);
        assert!(revision.shares_rows_with(&previous));
        assert!(matches!(
            revision.materialize_checkpoint(),
            PlaybackCheckpointMaterialization::Traversal(_)
        ));
    }

    #[test]
    fn coalescing_keeps_an_unwritten_row_revision_self_contained() {
        let mut sequence = Sequence::new(SourceId::fake(1));
        let (_, row_change) = sequence
            .apply_batch_with_change(
                Batch::new((1..=8).map(item).collect()),
                Placement::Replace { anchor_index: 0 },
            )
            .expect("replace rows");
        assert!(sequence.set_shuffle_seed(true, 17));
        let mut pending = PlaybackCheckpointRevision::capture(
            &sequence,
            CheckpointChange {
                expected_revision: row_change.expected_revision,
                rows_changed: true,
            },
        );
        let (_, traversal_change) = sequence
            .apply_batch_with_change(
                Batch::new((1..=8).map(item).collect()).with_shuffle_intent(29, true),
                Placement::Replace { anchor_index: 0 },
            )
            .expect("restart identical rows");
        let latest = PlaybackCheckpointRevision::capture(
            &sequence,
            CheckpointChange {
                expected_revision: traversal_change.expected_revision,
                rows_changed: traversal_change.rows_changed,
            },
        );

        pending.coalesce(latest);

        assert!(matches!(
            pending.materialize_checkpoint(),
            PlaybackCheckpointMaterialization::Full(_)
        ));
    }

    fn item(number: u32) -> BatchItem {
        BatchItem::new(
            Track::new(TrackData {
                id: TrackId::fake(number),
                album_id: Some(AlbumId::fake(1)),
                title: format!("Track {number}"),
                artist: "Artist".to_string(),
                album: "Album".to_string(),
                album_artwork: None,
                year: 2026,
                release_date: None,
                date_added: None,
                last_played: None,
                play_count: None,
                user_rating: None,
                duration_seconds: 180,
                favorite: false,
                disc_number: 1,
                track_number: number as u16,
                image_ref: None,
                local_artwork: None,
                musicbrainz_recording_id: None,
                musicbrainz_release_track_id: None,
                source_path: None,
                cue: None,
                source_format: None,
                comment: None,
                skip_count: None,
                bpm: None,
                relations: TrackRelations::default(),
            }),
            Provenance::Manual,
        )
    }

    fn full_checkpoint(sequence: &Sequence) -> PlaybackCheckpoint {
        PlaybackCheckpointRevision::capture(
            sequence,
            CheckpointChange {
                expected_revision: sequence.revision(),
                rows_changed: true,
            },
        )
        .materialize_full_checkpoint()
    }
}
