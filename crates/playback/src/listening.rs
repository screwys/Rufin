use library::{SourceKey, TrackKey};

use crate::{PlaybackMedia, RunId};

/// The immutable submission facts captured for one playback run.
///
/// This is intentionally narrower than a library item. Later library or
/// metadata changes must not rewrite a run that has already started.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ListeningTrack {
    pub source_key: SourceKey,
    pub track_key: Option<TrackKey>,
    pub track_object_id: String,
    pub recording_id: Option<String>,
    pub title: String,
    pub artists: Vec<String>,
    pub album: Option<String>,
    pub track_number: Option<i64>,
    pub disc_number: Option<i64>,
    pub duration_millis: u64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ActivityListen {
    pub play_id: String,
    pub track: ListeningTrack,
    pub started_at_unix_seconds: i64,
    pub local_period: String,
    pub listened_millis: u64,
    pub skipped: bool,
}

impl ListeningTrack {
    pub fn capture(source_key: SourceKey, track: &PlaybackMedia) -> Self {
        Self {
            source_key,
            track_key: track.track_key,
            track_object_id: track.track_object_id.clone(),
            recording_id: track.musicbrainz_recording_id.clone(),
            title: track.title.clone(),
            artists: vec![track.artist.clone()],
            album: (!track.album.trim().is_empty()).then(|| track.album.clone()),
            track_number: track.track_number.filter(|number| *number > 0),
            disc_number: track.disc_number.filter(|number| *number > 0),
            duration_millis: track.duration_millis.max(0) as u64,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RunEndReason {
    Completed,
    ManualSkip,
    Stopped,
    Replaced,
    Failed,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ListeningFact {
    Started {
        run: RunId,
        started_at_unix_seconds: i64,
        local_period: String,
        track: ListeningTrack,
    },
    Progress {
        run: RunId,
        audible_millis: u64,
        playhead_millis: u64,
    },
    Ended {
        run: RunId,
        reason: RunEndReason,
        audible_millis: u64,
        playhead_millis: u64,
    },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ListeningOutcome {
    pub play_id: String,
    pub run: RunId,
    pub source_key: SourceKey,
    pub track_key: Option<TrackKey>,
    pub local_period: String,
    pub qualified_plays: u32,
    pub skips: u32,
    pub last_played_at_unix_seconds: Option<i64>,
}

pub fn qualified_play_threshold_millis(duration_millis: u64) -> u64 {
    let duration_seconds = duration_millis / 1_000;
    let threshold_seconds = if duration_seconds <= 10 {
        duration_seconds
    } else {
        let half = duration_seconds / 2;
        if duration_seconds < 60 {
            half.max(5)
        } else {
            half.clamp(30, 240)
        }
    };
    threshold_seconds.saturating_mul(1_000)
}

pub fn external_scrobble_threshold_millis(duration_millis: u64) -> Option<u64> {
    if duration_millis <= 30_000 {
        return None;
    }
    Some((duration_millis / 2).min(240_000))
}

pub fn manual_end_is_skip(
    reason: RunEndReason,
    duration_millis: u64,
    audible_millis: u64,
    playhead_millis: u64,
) -> bool {
    if !matches!(reason, RunEndReason::ManualSkip | RunEndReason::Replaced) {
        return false;
    }
    audible_millis < qualified_play_threshold_millis(duration_millis)
        && duration_millis.saturating_sub(playhead_millis) > 5_000
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn qualified_play_threshold_preserves_short_half_and_clamped_law() {
        assert_eq!(qualified_play_threshold_millis(10_000), 10_000);
        assert_eq!(qualified_play_threshold_millis(20_000), 10_000);
        assert_eq!(qualified_play_threshold_millis(180_000), 90_000);
        assert_eq!(qualified_play_threshold_millis(900_000), 240_000);
    }

    #[test]
    fn external_scrobbles_retain_the_service_duration_law() {
        assert_eq!(external_scrobble_threshold_millis(30_000), None);
        assert_eq!(external_scrobble_threshold_millis(31_000), Some(15_500));
        assert_eq!(external_scrobble_threshold_millis(180_000), Some(90_000));
        assert_eq!(external_scrobble_threshold_millis(900_000), Some(240_000));
    }

    #[test]
    fn seeked_playhead_does_not_qualify_a_play_or_hide_an_early_skip() {
        assert!(manual_end_is_skip(
            RunEndReason::ManualSkip,
            180_000,
            4_000,
            170_000,
        ));
        assert!(!manual_end_is_skip(
            RunEndReason::ManualSkip,
            180_000,
            90_000,
            170_000,
        ));
    }

    #[test]
    fn natural_end_and_explicit_stop_are_not_skips() {
        for reason in [RunEndReason::Completed, RunEndReason::Stopped] {
            assert!(!manual_end_is_skip(reason, 180_000, 4_000, 10_000));
        }
    }
}
