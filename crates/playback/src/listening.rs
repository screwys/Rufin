use crate::{RepeatMode, RunId};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SourceReportPhase {
    Started,
    Progress,
    QualifiedPlay,
    Ended,
}

#[derive(Clone, Debug, PartialEq)]
pub struct SourceReportFact {
    pub transcoded: bool,
    pub run: RunId,
    pub session_identifier: String,
    pub queue_id: Option<u64>,
    pub queue_item_id: Option<u64>,
    pub media_uri: String,
    pub phase: SourceReportPhase,
    pub started_at_unix_seconds: i64,
    pub position_millis: u64,
    pub playback_rate: f64,
    pub duration_millis: Option<u64>,
    pub paused: bool,
    pub muted: bool,
    pub volume: f64,
    pub shuffle: bool,
    pub repeat_mode: RepeatMode,
    pub failed: bool,
}

/// The existing listen moves with playback; delivery jobs do not.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ContinuedListen {
    pub play_id: String,
    pub started_at_unix_seconds: Option<i64>,
    pub local_period: Option<String>,
    pub audible_millis: u64,
    pub qualified: bool,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ContinuationHeader {
    pub total: usize,
    pub current: usize,
    pub position_millis: u64,
    pub repeat: crate::RepeatMode,
    pub shuffled: bool,
    pub auto_dj: bool,
    pub auto_dj_refill_threshold: usize,
    pub playback_rate: f64,
    #[serde(default = "continuation_playing")]
    pub desired_playing: bool,
    pub listen: Option<ContinuedListen>,
}

fn continuation_playing() -> bool {
    true
}

/// Membership and resolved order share the player's compact, immutable arrays.
/// Transport reads metadata from the Store in bounded pages.
#[derive(Clone, Debug)]
pub struct Continuation {
    pub header: ContinuationHeader,
    pub queue: library::QueueRestore,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RunEndReason {
    Completed,
    ManualSkip,
    Stopped,
    Replaced,
    Failed,
}

pub fn qualified_play_threshold_millis(duration_millis: u64) -> u64 {
    if duration_millis <= 10_000 {
        duration_millis
    } else if duration_millis < 60_000 {
        (duration_millis / 2).max(5_000)
    } else {
        (duration_millis / 2).clamp(30_000, 240_000)
    }
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
        assert_eq!(qualified_play_threshold_millis(9_500), 9_500);
        assert_eq!(qualified_play_threshold_millis(31_000), 15_500);
        assert_eq!(qualified_play_threshold_millis(10_000), 10_000);
        assert_eq!(qualified_play_threshold_millis(20_000), 10_000);
        assert_eq!(qualified_play_threshold_millis(180_000), 90_000);
        assert_eq!(qualified_play_threshold_millis(900_000), 240_000);
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
