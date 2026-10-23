use lindera::{dictionary::load_fs_dictionary, mode::Mode, segmenter::Segmenter};
use std::{ops::Range, sync::Arc};

use crate::{JapaneseReading, KaraokeTiming, LyricsDocument, LyricsTiming, UserPreferences};

#[derive(Debug)]
pub struct PreparedLyrics {
    pub lines: Vec<PreparedLyricsLine>,
    pub pronunciation: Vec<PreparedLyricsLine>,
    pub timing: LyricsTiming,
}

#[derive(Debug)]
pub struct PreparedLyricsLine {
    pub reading: Option<Arc<JapaneseReading>>,
    pub pronunciation: Option<String>,
    pub cue_lines: Vec<PreparedCueLine>,
}

#[derive(Debug)]
pub struct PreparedCueLine {
    pub reading: Option<Arc<JapaneseReading>>,
    pub surface: Vec<PreparedLyricsSpan>,
    pub cues: Vec<Option<KaraokeTiming>>,
    pub furigana: Vec<Vec<KaraokeTiming>>,
    pub romanization: Vec<Vec<KaraokeTiming>>,
}

#[derive(Debug)]
pub struct PreparedLyricsSpan {
    pub range: Range<usize>,
    pub timings: Vec<KaraokeTiming>,
}

impl PreparedLyrics {
    pub fn prepare(
        document: &LyricsDocument,
        pronunciation: Option<&LyricsDocument>,
        preferences: &UserPreferences,
        dictionary: Option<&std::path::Path>,
        current: impl FnOnce() -> bool,
    ) -> Option<Self> {
        let mut reader = dictionary.map(|_| {
            crate::JAPANESE_READER
                .lock()
                .unwrap_or_else(|p| p.into_inner())
        });
        if !current() {
            return None;
        }
        if let (Some(reader), Some(path)) = (&mut reader, dictionary) {
            reader.get_or_insert_with(|| match load_fs_dictionary(path) {
                Ok(dictionary) => crate::JapaneseReaderState::Ready(Box::new(
                    Segmenter::new(Mode::Normal, dictionary, None).keep_whitespace(true),
                )),
                Err(error) => {
                    tracing::warn!(path = %path.display(), %error, "could not load Japanese readings dictionary");
                    crate::JapaneseReaderState::Unavailable
                }
            });
        }
        let tokenizer = reader.as_ref().and_then(|reader| match reader.as_ref() {
            Some(crate::JapaneseReaderState::Ready(tokenizer)) => Some(tokenizer.as_ref()),
            _ => None,
        });
        Some(Self {
            lines: prepare_lines(document, pronunciation, preferences, tokenizer),
            pronunciation: pronunciation
                .map(|document| prepare_lines(document, None, &UserPreferences::default(), None))
                .unwrap_or_default(),
            timing: LyricsTiming::new(&document.lines),
        })
    }
}

fn prepare_lines(
    document: &LyricsDocument,
    pronunciation: Option<&LyricsDocument>,
    preferences: &UserPreferences,
    tokenizer: Option<&lindera::segmenter::Segmenter>,
) -> Vec<PreparedLyricsLine> {
    let japanese = document.is_japanese_for_readings();
    let read = |text: &str, supplied: Option<&str>| {
        if !japanese || !preferences.show_furigana && !preferences.show_romanization {
            return None;
        }
        let supplied = preferences
            .show_furigana
            .then(|| {
                supplied
                    .and_then(|supplied| crate::japanese_reading_from_romanization(text, supplied))
            })
            .flatten();
        supplied
            .or_else(|| {
                crate::japanese_reading_for_language_options(
                    text,
                    Some("ja"),
                    preferences.show_furigana,
                    preferences.show_romanization,
                    tokenizer,
                )
            })
            .map(Arc::new)
    };
    document
        .lines
        .iter()
        .enumerate()
        .map(|(line_index, line)| {
            let supplied =
                crate::pronunciation_line_for(line, pronunciation).map(|line| line.text.as_str());
            let reading = read(&line.text, supplied);
            let cue_lines = line
                .cue_lines
                .iter()
                .map(|cue_line| {
                    let reading = if cue_line.text == line.text {
                        reading.clone()
                    } else {
                        read(
                            &cue_line.text,
                            supplied.filter(|_| line.cue_lines.len() == 1),
                        )
                    };
                    let cues = cue_line
                        .cues
                        .iter()
                        .enumerate()
                        .map(|(index, cue)| {
                            (
                                cue,
                                crate::effective_cue_end(
                                    &document.lines,
                                    line_index,
                                    cue_line,
                                    index,
                                ),
                            )
                        })
                        .collect::<Vec<_>>();
                    let timings = |range: Range<usize>| {
                        cues.iter()
                            .filter_map(|(cue, end)| {
                                KaraokeTiming::for_text_range(
                                    &cue_line.text,
                                    range.clone(),
                                    cue,
                                    *end,
                                )
                            })
                            .collect::<Vec<_>>()
                    };
                    let mut boundaries = vec![0, cue_line.text.len()];
                    for cue in &cue_line.cues {
                        boundaries.extend(
                            [cue.byte_start, cue.byte_end_exclusive]
                                .into_iter()
                                .filter(|boundary| *boundary < cue_line.text.len()),
                        );
                    }
                    let mut furigana = Vec::new();
                    let mut romanization = Vec::new();
                    if let Some(reading) = &reading {
                        let mut cursor = 0;
                        for segment in &reading.segments {
                            let range = cursor..cursor + segment.surface.len();
                            cursor = range.end;
                            boundaries.push(cursor);
                            furigana.push(if segment.furigana.is_some() {
                                timings(range)
                            } else {
                                Vec::new()
                            });
                        }
                        romanization = reading
                            .romanization_spans
                            .iter()
                            .map(|(range, _)| timings(range.clone()))
                            .collect();
                    }
                    boundaries.sort_unstable();
                    boundaries.dedup();
                    PreparedCueLine {
                        reading,
                        surface: boundaries
                            .windows(2)
                            .map(|pair| {
                                let range = pair[0]..pair[1];
                                PreparedLyricsSpan {
                                    timings: timings(range.clone()),
                                    range,
                                }
                            })
                            .collect(),
                        cues: cues
                            .iter()
                            .map(|(cue, end)| {
                                KaraokeTiming::for_text_range(
                                    &cue_line.text,
                                    cue.byte_start..cue.byte_end_exclusive,
                                    cue,
                                    *end,
                                )
                            })
                            .collect(),
                        furigana,
                        romanization,
                    }
                })
                .collect();
            PreparedLyricsLine {
                reading,
                pronunciation: supplied.map(str::to_owned),
                cue_lines,
            }
        })
        .collect()
}
