//! The sleep timer: pauses playback after a time, or after a number of track ends.

use std::time::{SystemTime, UNIX_EPOCH};

use playback::{SleepTimerChoice, SleepTimerView};

/// The last seconds of a time fade out.
pub(super) const FADE_MILLIS: u64 = 10_000;
const EXTEND_MILLIS: u64 = 5 * 60_000;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Enum))]
pub enum SleepTimerRequest {
    Start {
        choice: SleepTimerChoice,
        finish_track: bool,
    },
    /// Five more minutes, or one more track.
    Extend,
    FinishTrack {
        enabled: bool,
    },
    Off,
}

#[derive(Clone, Debug)]
pub(super) struct SleepTimer {
    choice: SleepTimerChoice,
    finish_track: bool,
    /// While a time runs: when it ends, on the monotonic clock and in Unix milliseconds.
    deadline: Option<(u64, i64)>,
    /// Track ends to wait for once the timer counts tracks.
    tracks_left: u32,
}

pub(super) enum SleepTimerDue {
    Waiting,
    /// The time ended with "finish the last track": playback stops when the playing track ends.
    FinishTrack,
    FadeOut {
        millis: u64,
    },
}

fn unix_millis() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_millis() as i64)
}

impl SleepTimer {
    pub(super) fn new(choice: SleepTimerChoice, finish_track: bool, now: u64) -> Option<Self> {
        let mut timer = Self {
            choice,
            finish_track,
            deadline: None,
            tracks_left: 0,
        };
        match choice {
            SleepTimerChoice::Minutes { minutes: 0 } | SleepTimerChoice::Tracks { tracks: 0 } => {
                return None;
            }
            SleepTimerChoice::Minutes { minutes } => {
                timer.run_for(u64::from(minutes) * 60_000, now)
            }
            SleepTimerChoice::Tracks { tracks } => timer.tracks_left = tracks,
        }
        Some(timer)
    }

    fn run_for(&mut self, millis: u64, now: u64) {
        self.deadline = Some((now + millis, unix_millis() + millis as i64));
        self.tracks_left = 0;
    }

    pub(super) fn extend(&mut self, now: u64) {
        match (self.choice, self.deadline) {
            (SleepTimerChoice::Tracks { .. }, _) => self.tracks_left += 1,
            (SleepTimerChoice::Minutes { .. }, Some((at, unix))) => {
                self.deadline = Some((at + EXTEND_MILLIS, unix + EXTEND_MILLIS as i64));
            }
            // Waiting for the last track to end: five minutes from now.
            (SleepTimerChoice::Minutes { .. }, None) => self.run_for(EXTEND_MILLIS, now),
        }
    }

    pub(super) fn set_finish_track(&mut self, enabled: bool, now: u64) {
        if self.finish_track == enabled || !matches!(self.choice, SleepTimerChoice::Minutes { .. })
        {
            return;
        }
        self.finish_track = enabled;
        // The time ended already: it fades out now.
        if !enabled && self.deadline.is_none() {
            self.run_for(0, now);
        }
    }

    fn due_at(&self) -> Option<u64> {
        let (at, _) = self.deadline?;
        Some(if self.finish_track {
            at
        } else {
            at.saturating_sub(FADE_MILLIS)
        })
    }

    /// How long the worker may wait before the timer needs it.
    pub(super) fn wait_millis(&self, now: u64) -> Option<u64> {
        self.due_at().map(|at| at.saturating_sub(now))
    }

    pub(super) fn due(&mut self, now: u64) -> SleepTimerDue {
        let (Some(due), Some((at, _))) = (self.due_at(), self.deadline) else {
            return SleepTimerDue::Waiting;
        };
        if now < due {
            return SleepTimerDue::Waiting;
        }
        if self.finish_track {
            self.deadline = None;
            self.tracks_left = 1;
            return SleepTimerDue::FinishTrack;
        }
        SleepTimerDue::FadeOut {
            millis: at.saturating_sub(now),
        }
    }

    /// Counts a track end; true when it was the last one and playback stops.
    pub(super) fn track_ended(&mut self) -> bool {
        if self.deadline.is_some() || self.tracks_left == 0 {
            return false;
        }
        self.tracks_left -= 1;
        self.tracks_left == 0
    }

    /// The playing track is the last one: the next is not prepared, so nothing starts it.
    pub(super) fn stops_after_current(&self) -> bool {
        self.deadline.is_none() && self.tracks_left == 1
    }

    /// `current_left` is what remains of the playing track and `upcoming` the following
    /// tracks' durations, all in playback time; paused, tracks have no end time.
    pub(super) fn view(
        &self,
        playing: bool,
        current_left: u64,
        upcoming: impl Iterator<Item = Option<u64>>,
        rate: f64,
    ) -> SleepTimerView {
        let ends_at_unix_millis = match self.deadline {
            Some((_, unix)) => Some(unix),
            None if !playing => None,
            None => upcoming
                .take(self.tracks_left.saturating_sub(1) as usize)
                .try_fold(current_left, |total, duration| Some(total + duration?))
                .filter(|_| self.tracks_left > 0)
                .map(|total| unix_millis() + (total as f64 / rate.max(0.1)) as i64),
        };
        SleepTimerView {
            choice: self.choice,
            finish_track: self.finish_track,
            tracks_left: self.tracks_left,
            ends_at_unix_millis,
        }
    }
}

/// One tap starts these, in minutes and in tracks.
pub const SLEEP_TIMER_MINUTES: [u32; 8] = [5, 10, 15, 20, 30, 45, 60, 90];
pub const SLEEP_TIMER_TRACKS: [u32; 7] = [1, 2, 3, 5, 10, 15, 20];

#[derive(Clone, Debug, Eq, PartialEq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct SleepTimerPresets {
    pub minutes: Vec<u32>,
    pub tracks: Vec<u32>,
}

#[cfg_attr(feature = "bindings", uniffi::export)]
pub fn sleep_timer_presets() -> SleepTimerPresets {
    SleepTimerPresets {
        minutes: SLEEP_TIMER_MINUTES.to_vec(),
        tracks: SLEEP_TIMER_TRACKS.to_vec(),
    }
}

/// What every client shows of a running timer.
#[derive(Clone, Debug, Eq, PartialEq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct SleepTimerText {
    /// The large value: the time left, or the tracks left.
    pub countdown: String,
    /// When playback pauses.
    pub caption: String,
    /// The short value for the player's button.
    pub short: String,
    pub extend: String,
}

#[cfg_attr(feature = "bindings", uniffi::export)]
pub fn sleep_timer_text(timer: SleepTimerView, now_unix_millis: i64) -> SleepTimerText {
    use localization::{tr, tr_with, trn_with};
    let left = timer
        .ends_at_unix_millis
        .map(|end| (end.saturating_sub(now_unix_millis).max(0) as u64).div_ceil(1_000));
    let at = timer.ends_at_unix_millis.and_then(clock_time);
    let time_left = left.map(countdown).unwrap_or_default();
    let counting_tracks = timer.tracks_left > 1 || left.is_none();
    let tracks = trn_with(
        "{count} track",
        "{count} tracks",
        u64::from(timer.tracks_left),
        &[("count", &timer.tracks_left.to_string())],
    );
    let caption = match (&timer.choice, timer.tracks_left, at) {
        (_, 1, _) => tr("Pauses when this track ends"),
        (SleepTimerChoice::Tracks { .. }, _, Some(at)) => {
            tr_with("Ends around {time}", &[("time", &at)])
        }
        (SleepTimerChoice::Tracks { .. }, _, None) => String::new(),
        (SleepTimerChoice::Minutes { .. }, _, Some(at)) if timer.finish_track => {
            tr_with("Finishes the track playing at {time}", &[("time", &at)])
        }
        (SleepTimerChoice::Minutes { .. }, _, Some(at)) => {
            tr_with("Pauses at {time}", &[("time", &at)])
        }
        (SleepTimerChoice::Minutes { .. }, _, None) => String::new(),
    };
    SleepTimerText {
        countdown: if counting_tracks {
            tracks.clone()
        } else {
            time_left.clone()
        },
        caption,
        short: if counting_tracks { tracks } else { time_left },
        extend: match timer.choice {
            SleepTimerChoice::Minutes { .. } => tr("+5 min"),
            SleepTimerChoice::Tracks { .. } => tr("+1 track"),
        },
    }
}

fn countdown(seconds: u64) -> String {
    let (hours, minutes, seconds) = (seconds / 3_600, seconds / 60 % 60, seconds % 60);
    if hours > 0 {
        format!("{hours}:{minutes:02}:{seconds:02}")
    } else {
        format!("{minutes}:{seconds:02}")
    }
}

/// The time of day in the device's zone; glib's local zone is UTC on Android.
fn clock_time(unix_millis: i64) -> Option<String> {
    use chrono::TimeZone;
    let time = chrono::Local.timestamp_millis_opt(unix_millis).single()?;
    Some(time.format("%H:%M").to_string())
}
