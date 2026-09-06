use crate::shell::Shell;
use gtk::glib;
use lyrics::{LyricsCue, LyricsCueLine, LyricsLine};
use playback::{PlaybackOutput, TransportStatus};
use std::cmp::Reverse;
use std::collections::BinaryHeap;
use std::rc::Rc;
use std::time::Duration;

#[derive(Clone)]
pub(super) struct KaraokeTiming {
    start_millis: u64,
    end_millis: Option<u64>,
    from: f64,
    to: f64,
    weight: f64,
}

impl KaraokeTiming {
    pub(super) fn for_text_range(
        text: &str,
        range: std::ops::Range<usize>,
        cue: &LyricsCue,
        end_millis: Option<u64>,
    ) -> Option<Self> {
        let start = range.start.max(cue.byte_start);
        let end = range.end.min(cue.byte_end_exclusive);
        if start >= end {
            return None;
        }
        let cue_length = text
            .get(cue.byte_start..cue.byte_end_exclusive)?
            .chars()
            .count() as f64;
        let length = text.get(range)?.chars().count() as f64;
        let offset = text.get(cue.byte_start..start)?.chars().count() as f64;
        let overlap = text.get(start..end)?.chars().count() as f64;
        Some(Self {
            start_millis: cue.start_millis,
            end_millis,
            from: offset / cue_length,
            to: (offset + overlap) / cue_length,
            weight: overlap / length,
        })
    }

    pub(super) fn progress(&self, position_millis: i128) -> f64 {
        let start = i128::from(self.start_millis);
        let progress = if position_millis <= start {
            0.0
        } else if let Some(end) = self.end_millis.filter(|end| *end > self.start_millis) {
            ((position_millis - start) as f64 / (i128::from(end) - start) as f64).clamp(0.0, 1.0)
        } else {
            1.0
        };
        ((progress - self.from) / (self.to - self.from)).clamp(0.0, 1.0) * self.weight
    }
}

#[derive(Default)]
pub(super) struct LyricsTiming {
    line_starts: Vec<u64>,
    cue_boundaries: Vec<u64>,
    // Changes of the earliest active cue end, including overlapping and simultaneous cues.
    active_ends: Vec<(u64, u64)>,
}

impl LyricsTiming {
    pub(super) fn new(lines: &[LyricsLine]) -> Self {
        let mut timing = Self::default();
        let mut intervals = Vec::new();
        let mut changes = Vec::new();
        for (line_index, line) in lines.iter().enumerate() {
            timing.line_starts.extend(line.start_millis);
            for cue_line in &line.cue_lines {
                for (cue_index, cue) in cue_line.cues.iter().enumerate() {
                    timing.cue_boundaries.push(cue.start_millis);
                    timing.cue_boundaries.extend(cue.end_millis);
                    if let Some(end) = effective_cue_end(lines, line_index, cue_line, cue_index)
                        && cue.start_millis < end
                    {
                        intervals.push((cue.start_millis, end));
                        changes.extend([cue.start_millis, end]);
                    }
                }
            }
        }
        timing.line_starts.sort_unstable();
        timing.line_starts.dedup();
        timing.cue_boundaries.extend(&timing.line_starts);
        timing.cue_boundaries.sort_unstable();
        timing.cue_boundaries.dedup();
        intervals.sort_unstable();
        changes.sort_unstable();
        changes.dedup();
        let mut intervals = intervals.into_iter().peekable();
        let mut ends = BinaryHeap::new();
        for position in changes {
            while intervals
                .peek()
                .is_some_and(|(start, _)| *start <= position)
            {
                let (_, end) = intervals.next().unwrap();
                ends.push(Reverse(end));
            }
            while ends.peek().is_some_and(|Reverse(end)| *end <= position) {
                ends.pop();
            }
            if let Some(&Reverse(end)) = ends.peek()
                && timing
                    .active_ends
                    .last()
                    .is_none_or(|&(_, previous)| previous != end)
            {
                timing.active_ends.push((position, end));
            }
        }
        timing
    }

    pub(super) fn next_after(&self, position: i128, karaoke: bool) -> Option<u64> {
        if karaoke {
            let active = self
                .active_ends
                .partition_point(|&(start, _)| i128::from(start) <= position);
            if let Some((_, end)) = active.checked_sub(1).map(|index| self.active_ends[index])
                && position < i128::from(end)
            {
                return Some(
                    u64::try_from(position.max(0))
                        .unwrap_or_default()
                        .saturating_add(16)
                        .min(end),
                );
            }
        }
        let boundaries = if karaoke {
            &self.cue_boundaries
        } else {
            &self.line_starts
        };
        boundaries
            .get(boundaries.partition_point(|&time| i128::from(time) <= position))
            .copied()
    }
}

pub(super) fn effective_cue_end(
    lines: &[LyricsLine],
    line_index: usize,
    cue_line: &LyricsCueLine,
    cue_index: usize,
) -> Option<u64> {
    cue_line.cues[cue_index]
        .end_millis
        .or_else(|| cue_line.cues.get(cue_index + 1).map(|cue| cue.start_millis))
        .or(cue_line.end_millis)
        .or(lines[line_index].end_millis)
        .or_else(|| lines.get(line_index + 1).and_then(|line| line.start_millis))
}

impl Shell {
    pub(crate) fn cancel_scheduled_lyrics_highlight(&self) {
        let Some(lyrics) = self.selected_lyrics() else {
            return;
        };
        if let Some(source) = lyrics.timing_source.borrow_mut().take() {
            source.remove();
        }
    }
    pub(crate) fn schedule_next_lyrics_highlight(self: &Rc<Self>, position_millis: u64) {
        if !self.lyrics_surface_visible() {
            return;
        }
        let follows_local_clock = self.selected_playback().as_deref().is_some_and(|player| {
            lyrics_follow_local_clock(player.transport.state, &player.controls.playback_output)
        });
        if !follows_local_clock {
            return;
        }

        if self.visible_lyrics().is_none() {
            return;
        }
        let lyrics_position_millis = self.lyrics_position_millis(position_millis);
        let Some(lyrics) = self.selected_lyrics() else {
            return;
        };
        let karaoke = self.settings.current.borrow().lyrics.karaoke_mode;
        let Some(next_position_millis) = lyrics
            .timing
            .borrow()
            .next_after(lyrics_position_millis, karaoke)
        else {
            return;
        };
        let Ok(delay_millis) =
            u64::try_from(i128::from(next_position_millis) - lyrics_position_millis)
        else {
            return;
        };
        let next_playback_position_millis = position_millis.saturating_add(delay_millis);
        drop(lyrics);

        let shell = Rc::clone(self);
        let source = glib::timeout_add_local_once(Duration::from_millis(delay_millis), move || {
            let Some(lyrics) = shell.selected_lyrics() else {
                return;
            };
            let _source = lyrics.timing_source.borrow_mut().take();
            drop(lyrics);
            shell.update_lyrics_highlight_at(next_playback_position_millis);
        });
        let Some(lyrics) = self.selected_lyrics() else {
            source.remove();
            return;
        };
        if let Some(previous_source) = lyrics.timing_source.borrow_mut().replace(source) {
            previous_source.remove();
        }
    }
}

fn lyrics_follow_local_clock(state: TransportStatus, output: &PlaybackOutput) -> bool {
    state == TransportStatus::Playing && output.is_local()
}

#[cfg(test)]
mod tests {
    use lyrics::{LyricsCue, LyricsCueLine, LyricsLine};
    use playback::{PlaybackOutput, RemoteOutput, RemoteOutputProtocol, TransportStatus};

    use super::{KaraokeTiming, LyricsTiming, effective_cue_end, lyrics_follow_local_clock};

    #[test]
    fn compound_reading_keeps_each_characters_original_timing_after_seeks() {
        let text = "正気";
        let cues = [
            LyricsCue {
                text: "正".into(),
                byte_start: 0,
                byte_end_exclusive: 3,
                start_millis: 1000,
                end_millis: Some(1100),
            },
            LyricsCue {
                text: "気".into(),
                byte_start: 3,
                byte_end_exclusive: 6,
                start_millis: 1100,
                end_millis: Some(1500),
            },
        ];
        let timings = cues
            .iter()
            .map(|cue| {
                KaraokeTiming::for_text_range(text, 0..text.len(), cue, cue.end_millis).unwrap()
            })
            .collect::<Vec<_>>();
        for (position, expected) in [
            (1000, 0.0),
            (1050, 0.25),
            (1100, 0.5),
            (1300, 0.75),
            (1500, 1.0),
            (900, 0.0),
            (1300, 0.75),
        ] {
            assert_eq!(
                timings
                    .iter()
                    .map(|timing| timing.progress(position))
                    .sum::<f64>(),
                expected
            );
        }
        let second =
            KaraokeTiming::for_text_range(text, 3..6, &cues[1], cues[1].end_millis).unwrap();
        assert_eq!(second.progress(1100), 0.0);
        assert_eq!(second.progress(1300), 0.5);
    }

    #[test]
    fn one_cue_advances_through_reading_segments_using_characters_not_bytes() {
        let text = "A正気";
        let cue = LyricsCue {
            text: text.into(),
            byte_start: 0,
            byte_end_exclusive: text.len(),
            start_millis: 1000,
            end_millis: Some(1600),
        };
        let compound =
            KaraokeTiming::for_text_range(text, 1..text.len(), &cue, cue.end_millis).unwrap();
        assert_eq!(compound.progress(1200), 0.0);
        assert!((compound.progress(1400) - 0.5).abs() < 1e-9);
        assert_eq!(compound.progress(1600), 1.0);
    }

    fn line(start: Option<u64>, end: Option<u64>, cues: &[(u64, Option<u64>)]) -> LyricsLine {
        LyricsLine {
            text: "line".into(),
            start_millis: start,
            end_millis: end,
            cue_lines: vec![LyricsCueLine {
                text: "line".into(),
                start_millis: start,
                end_millis: None,
                agent_id: None,
                cues: cues
                    .iter()
                    .map(|&(start_millis, end_millis)| LyricsCue {
                        text: "cue".into(),
                        start_millis,
                        end_millis,
                        byte_start: 0,
                        byte_end_exclusive: 3,
                    })
                    .collect(),
            }],
        }
    }

    // Reference semantics of the former document scan, independent of the prepared index.
    fn scanned_next(lines: &[LyricsLine], position: i128) -> Option<u64> {
        let mut active_end = None;
        let mut boundary = None;
        let mut consider = |time: u64| {
            if i128::from(time) > position {
                boundary = Some(boundary.map_or(time, |previous: u64| previous.min(time)));
            }
        };
        for (line_index, line) in lines.iter().enumerate() {
            if let Some(start) = line.start_millis {
                consider(start);
            }
            for cue_line in &line.cue_lines {
                for (cue_index, cue) in cue_line.cues.iter().enumerate() {
                    consider(cue.start_millis);
                    if let Some(end) = cue.end_millis {
                        consider(end);
                    }
                    if let Some(end) = effective_cue_end(lines, line_index, cue_line, cue_index)
                        && i128::from(cue.start_millis) <= position
                        && position < i128::from(end)
                    {
                        active_end =
                            Some(active_end.map_or(end, |previous: u64| previous.min(end)));
                    }
                }
            }
        }
        active_end
            .map(|end| {
                u64::try_from(position.max(0))
                    .unwrap_or_default()
                    .saturating_add(16)
                    .min(end)
            })
            .or(boundary)
    }

    #[test]
    fn prepared_timing_preserves_boundaries_after_arbitrary_seeks() {
        let mut lines = vec![
            line(
                Some(10),
                Some(70),
                &[(10, Some(55)), (20, Some(35)), (35, None), (50, None)],
            ),
            line(Some(30), None, &[(30, Some(45)), (30, Some(60))]),
            line(Some(80), None, &[(80, None)]),
            line(Some(100), None, &[(100, None)]),
            line(
                Some(u64::MAX - 10),
                Some(u64::MAX),
                &[(u64::MAX - 10, None)],
            ),
        ];
        lines[0]
            .cue_lines
            .push(line(None, None, &[(15, Some(65))]).cue_lines.remove(0));
        lines[2].cue_lines[0].end_millis = Some(90);
        let timing = LyricsTiming::new(&lines);
        for position in (-20..150).rev().chain([
            i128::from(u64::MAX - 11),
            i128::from(u64::MAX - 10),
            i128::from(u64::MAX - 1),
            i128::from(u64::MAX),
            i128::MAX,
        ]) {
            assert_eq!(
                timing.next_after(position, true),
                scanned_next(&lines, position),
                "position={position}"
            );
        }
    }

    #[test]
    fn missing_cue_ends_keep_the_existing_fallback_order() {
        let mut lines = vec![
            line(Some(10), Some(70), &[(10, None), (20, None)]),
            line(Some(90), None, &[]),
        ];
        lines[0].cue_lines[0].end_millis = Some(50);
        assert_eq!(
            effective_cue_end(&lines, 0, &lines[0].cue_lines[0], 0),
            Some(20)
        );
        assert_eq!(
            effective_cue_end(&lines, 0, &lines[0].cue_lines[0], 1),
            Some(50)
        );
        lines[0].cue_lines[0].end_millis = None;
        assert_eq!(
            effective_cue_end(&lines, 0, &lines[0].cue_lines[0], 1),
            Some(70)
        );
        lines[0].end_millis = None;
        assert_eq!(
            effective_cue_end(&lines, 0, &lines[0].cue_lines[0], 1),
            Some(90)
        );
        lines.pop();
        assert_eq!(LyricsTiming::new(&lines).next_after(20, true), None);
    }

    #[test]
    fn karaoke_off_waits_for_lines_including_blank_lines() {
        let mut lines = vec![
            line(Some(1_000), Some(4_000), &[(1_000, Some(2_000))]),
            line(Some(5_000), None, &[]),
            line(Some(9_000), None, &[]),
        ];
        lines[1].text.clear();
        let timing = LyricsTiming::new(&lines);
        assert_eq!(timing.next_after(-250, false), Some(1_000));
        assert_eq!(timing.next_after(1_000, true), Some(1_016));
        assert_eq!(timing.next_after(1_999, true), Some(2_000));
        assert_eq!(timing.next_after(1_000, false), Some(5_000));
        assert_eq!(timing.next_after(4_999, false), Some(5_000));
        assert_eq!(timing.next_after(5_000, false), Some(9_000));
        assert_eq!(timing.next_after(9_000, false), None);
        for karaoke in [false, true] {
            assert_eq!(
                LyricsTiming::new(&[line(None, None, &[])]).next_after(0, karaoke),
                None
            );
            assert_eq!(LyricsTiming::default().next_after(-1, karaoke), None);
        }
    }

    #[test]
    fn remote_lyrics_wait_for_receiver_positions() {
        let remote = PlaybackOutput::Remote(RemoteOutput {
            id: "renderer".to_string(),
            name: "Renderer".to_string(),
            protocol: RemoteOutputProtocol::Upnp,
        });

        assert!(lyrics_follow_local_clock(
            TransportStatus::Playing,
            &PlaybackOutput::Local,
        ));
        assert!(!lyrics_follow_local_clock(
            TransportStatus::Playing,
            &remote,
        ));
    }
}
