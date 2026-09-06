use crate::{QueueItem, RunId};

#[derive(Clone, Debug, PartialEq)]
pub struct ActivityListen {
    pub play_id: String,
    pub item: QueueItem,
    pub started_at_unix_seconds: i64,
    pub local_period: String,
    pub listened_millis: u64,
    pub skipped: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RunEndReason {
    Completed,
    ManualSkip,
    Stopped,
    Replaced,
    Failed,
}

#[derive(Clone, Debug, PartialEq)]
pub enum ListeningFact {
    Started {
        run: RunId,
        started_at_unix_seconds: i64,
        local_period: String,
        item: Box<QueueItem>,
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
