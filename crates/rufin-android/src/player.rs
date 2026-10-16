use rufin_core::settings::visualizer::{
    VisualizerPeak, visualizer_accent_gradient, visualizer_bar_levels, visualizer_column_geometry,
    visualizer_row_geometry, visualizer_smoothed_level, visualizer_smoothing_weights,
};
use std::sync::Arc;
use std::sync::Mutex;

use rufin_core::runtime::{ProductHandles, VisualizerPublication};

use crate::browse::AndroidArtistLink;
use crate::host::{AndroidError, error};

#[derive(uniffi::Record)]
pub struct AndroidPlayerMetadata {
    pub media_uri: String,
    pub source_id: Option<String>,
    pub artist_text: String,
    pub artist_links: Vec<AndroidArtistLink>,
    pub album_text: String,
    pub album_links: Vec<AndroidArtistLink>,
}

#[derive(uniffi::Record)]
pub struct AndroidRgb {
    pub red: f32,
    pub green: f32,
    pub blue: f32,
}

impl From<[f32; 3]> for AndroidRgb {
    fn from(value: [f32; 3]) -> Self {
        Self {
            red: value[0],
            green: value[1],
            blue: value[2],
        }
    }
}

#[derive(uniffi::Record)]
pub struct AndroidVisualizerDrawing {
    pub bars: Vec<f64>,
    pub peaks: Vec<f64>,
    pub cell: f64,
    pub rows: u32,
    pub row_height: f64,
    pub gap: f64,
    pub style: String,
    pub opacity: f64,
    pub fps_limit: u32,
    pub start_color: AndroidRgb,
    pub end_color: AndroidRgb,
}

#[derive(Default)]
struct VisualizerDrawingState {
    levels: Vec<f64>,
    peaks: Vec<VisualizerPeak>,
    last_frame: Option<u64>,
}

#[derive(uniffi::Object)]
pub struct AndroidPlayer {
    products: ProductHandles,
    visualizer: tokio::sync::watch::Receiver<Arc<VisualizerPublication>>,
    drawing: Mutex<VisualizerDrawingState>,
    lyrics_events: tokio::sync::Mutex<async_channel::Receiver<rufin_core::lyrics::LyricsEvent>>,
    waveform: tokio::sync::Mutex<async_channel::Receiver<rufin_core::runtime::WaveformProjection>>,
}

#[uniffi::export]
impl AndroidPlayer {
    pub async fn next_waveform(
        &self,
    ) -> Result<Arc<rufin_core::runtime::WaveformProjection>, AndroidError> {
        Ok(Arc::new(
            self.waveform.lock().await.recv().await.map_err(error)?,
        ))
    }

    pub async fn next_lyrics_event(
        &self,
    ) -> Result<Arc<rufin_core::lyrics::LyricsEvent>, AndroidError> {
        Ok(Arc::new(
            self.lyrics_events
                .lock()
                .await
                .recv()
                .await
                .map_err(error)?,
        ))
    }
}

impl AndroidPlayer {
    pub(crate) fn new(
        products: &ProductHandles,
        frames: async_channel::Receiver<VisualizerPublication>,
        lyrics_events: async_channel::Receiver<rufin_core::lyrics::LyricsEvent>,
        waveform: async_channel::Receiver<rufin_core::runtime::WaveformProjection>,
    ) -> Self {
        let (sender, visualizer) = tokio::sync::watch::channel(Arc::new(VisualizerPublication {
            run: playback::RunId::new(0),
            levels: Vec::new(),
        }));
        products.runtime.spawn(async move {
            while let Ok(frame) = frames.recv().await {
                sender.send_replace(Arc::new(frame));
            }
        });
        let (events, operations) = async_channel::unbounded();
        products.runtime.spawn(async move {
            while let Ok(event) = lyrics_events.recv().await {
                if !matches!(event, rufin_core::lyrics::LyricsEvent::Current(_))
                    && events.send(event).await.is_err()
                {
                    break;
                }
            }
        });
        Self {
            products: products.clone(),
            visualizer,
            drawing: Mutex::new(VisualizerDrawingState::default()),
            lyrics_events: tokio::sync::Mutex::new(operations),
            waveform: tokio::sync::Mutex::new(waveform),
        }
    }
}

fn reading_for(
    text: &str,
    language: Option<&str>,
    supplied: Option<&str>,
    settings: &lyrics::Settings,
) -> Option<lyrics::JapaneseReading> {
    if !settings.preferences.show_furigana && !settings.preferences.show_romanization {
        return None;
    }
    let reading = if settings.preferences.show_furigana {
        supplied.and_then(|pronunciation| {
            lyrics::japanese_reading_from_romanization(text, pronunciation)
        })
    } else {
        None
    };
    reading.or_else(|| {
        lyrics::japanese_reading_for_language_options(
            text,
            language,
            settings.preferences.show_furigana,
            settings.preferences.show_romanization,
        )
    })
}

#[uniffi::export]
impl AndroidPlayer {
    pub async fn metadata_links(
        &self,
        media_uri: String,
        origin_route: Option<rufin_core::route::Route>,
    ) -> Result<Option<AndroidPlayerMetadata>, AndroidError> {
        let database = self.products.library.clone();
        self.products
            .runtime
            .spawn(async move {
                use rufin_core::route::Route;
                use rufin_core::route::detail_links::{
                    DetailLinks, album_artist_links, metadata_links, track_artist_links,
                };
                use rufin_core::settings::layout::LibraryField;
                let cancellation = library::ReadCancellation::new();
                let entity = library::source_entity_parts(&media_uri);
                let (artists, album, source_id) = match entity {
                    Some((source, kind, _)) if kind == "album" => {
                        let library::CatalogRows::Albums(mut rows) = library::CatalogQuery::uris(
                            library::CatalogKind::Album,
                            vec![media_uri.clone()],
                        )
                        .rows(&database, 0, 1, &cancellation)
                        .await
                        .map_err(error)?
                        else {
                            unreachable!("album query")
                        };
                        let Some(album) = rows.pop() else {
                            return Ok(None);
                        };
                        (
                            album_artist_links(&album),
                            DetailLinks::route(
                                &album.metadata.title,
                                Some(Route::AlbumDetail(media_uri.clone())),
                            ),
                            Some(source.to_string()),
                        )
                    }
                    Some((source, kind, _)) if kind == "artist" => {
                        let library::CatalogRows::Artists(mut rows) = library::CatalogQuery::uris(
                            library::CatalogKind::Artist,
                            vec![media_uri.clone()],
                        )
                        .rows(&database, 0, 1, &cancellation)
                        .await
                        .map_err(error)?
                        else {
                            unreachable!("artist query")
                        };
                        let Some(artist) = rows.pop() else {
                            return Ok(None);
                        };
                        let name = artist.metadata.name;
                        let route = match origin_route {
                            Some(Route::AlbumArtistDetail(uri)) if uri == media_uri => {
                                Route::AlbumArtistDetail(uri)
                            }
                            _ => Route::ArtistDetail(media_uri.clone()),
                        };
                        (
                            DetailLinks::route(&name, Some(route)),
                            DetailLinks::default(),
                            Some(source.to_string()),
                        )
                    }
                    _ => {
                        let Some(track) = library::CatalogQuery::uris(
                            library::CatalogKind::Track,
                            vec![(&media_uri).to_string()],
                        )
                        .rows(&database, 0, 1, &cancellation)
                        .await
                        .map(|rows| match rows {
                            library::CatalogRows::Tracks(mut rows) => rows.pop(),
                            _ => unreachable!("typed catalog projection"),
                        })
                        .map_err(error)?
                        else {
                            return Ok(None);
                        };
                        (
                            track_artist_links(&track),
                            metadata_links(
                                LibraryField::Album,
                                &track.metadata.album,
                                track.links.album_media_uri.as_deref(),
                                &track.links.artists,
                                &track.links.album_artists,
                            ),
                            track.source_id,
                        )
                    }
                };
                let mut artist_links = AndroidArtistLink::from_detail_links(&artists)?;
                let mut album_links = AndroidArtistLink::from_detail_links(&album)?;
                let media_uris: Vec<_> = artists
                    .spans()
                    .iter()
                    .chain(album.spans())
                    .map(|link| match &link.route {
                        rufin_core::route::Route::ArtistDetail(uri)
                        | rufin_core::route::Route::AlbumArtistDetail(uri)
                        | rufin_core::route::Route::AlbumDetail(uri) => uri.clone(),
                        _ => unreachable!("Artist and album metadata links"),
                    })
                    .collect();
                let mut bindings = Vec::with_capacity(media_uris.len());
                for window in media_uris.chunks(128) {
                    bindings.extend(
                        database
                            .artwork_bindings(window, &cancellation)
                            .await
                            .map_err(error)?,
                    );
                }
                for (link, binding) in artist_links
                    .iter_mut()
                    .chain(&mut album_links)
                    .zip(bindings)
                {
                    link.artwork_identity = binding;
                }
                Ok(Some(AndroidPlayerMetadata {
                    media_uri,
                    source_id,
                    artist_text: artists.display_text().to_string(),
                    artist_links,
                    album_text: album.display_text().to_string(),
                    album_links,
                }))
            })
            .await
            .map_err(error)?
    }

    pub fn current_lyrics(
        &self,
        source: Arc<rufin_core::lyrics::CurrentLyrics>,
    ) -> AndroidLyricsState {
        use rufin_core::lyrics::{CurrentLyrics, CurrentLyricsContent};
        let media = self
            .products
            .playback
            .updates
            .current()
            .and_then(|view| view.transport.current.clone());
        let stored = self.products.settings.load();
        let settings = stored.lyrics;
        if !settings.preferences.show_furigana && !settings.preferences.show_romanization {
            lyrics::release_japanese_reader();
        }
        let mut state = AndroidLyricsState {
            snapshot: None,
            current_media_id: media.as_ref().map(|media| media.id.clone()),
            media_id: None,
            writable: false,
            clearable: false,
            loading: matches!(source.as_ref(), CurrentLyrics::Loading { .. }),
            instrumental: false,
            document: None,
        };
        let (media_id, content) = match source.as_ref() {
            CurrentLyrics::Cleared => return state,
            CurrentLyrics::Loading { media_id } => (media_id, None),
            CurrentLyrics::Ready {
                media_id,
                content,
                origin,
            } => {
                state.writable = self.products.lyrics.current_writable(media_id);
                state.clearable = media.as_ref().is_some_and(|media| {
                    settings.can_suppress_auto_lyrics(
                        stored.application.private_mode,
                        &media.row.track.media_uri,
                        *origin,
                    )
                });
                (media_id, content.as_ref())
            }
        };
        state.media_id = Some(media_id.clone());
        state.instrumental = matches!(content, Some(CurrentLyricsContent::Instrumental));
        let Some(CurrentLyricsContent::Document {
            document,
            pronunciation,
        }) = content
        else {
            return state;
        };
        let readings = document.is_japanese_for_readings();
        if readings
            && (settings.preferences.show_furigana || settings.preferences.show_romanization)
        {
            if let lyrics::JapaneseDictionaryStatus::Ready(path) =
                self.products.lyrics.japanese_dictionary(false)
            {
                lyrics::prepare_japanese_reader(&path);
            }
        }
        let mut projected = AndroidLyricsDocument::from(document.as_ref());
        let mut highlight_timings: Vec<Vec<_>> = if settings.preferences.karaoke_mode {
            document.lines.iter().map(|_| Vec::new()).collect()
        } else {
            Vec::new()
        };
        for (index, line) in document.lines.iter().enumerate() {
            let supplied = lyrics::pronunciation_line_for(line, pronunciation.as_deref());
            let output = &mut projected.lines[index];
            output.pronunciation = supplied.map(|line| line.text.clone());
            if readings {
                output.reading = reading_for(
                    &line.text,
                    document.language.as_deref(),
                    supplied.map(|line| line.text.as_str()),
                    &settings,
                )
                .map(Into::into);
            }
            for (cue_index, cue_line) in line.cue_lines.iter().enumerate() {
                let supplied = if line.cue_lines.len() == 1 {
                    supplied.map(|line| line.text.as_str())
                } else {
                    None
                };
                let cues: Vec<_> = if settings.preferences.karaoke_mode {
                    cue_line
                        .cues
                        .iter()
                        .enumerate()
                        .map(|(cue_index, cue)| {
                            (
                                cue,
                                lyrics::effective_cue_end(
                                    &document.lines,
                                    index,
                                    cue_line,
                                    cue_index,
                                ),
                            )
                        })
                        .collect()
                } else {
                    Vec::new()
                };
                for (cue_part, (cue, end)) in cues.iter().enumerate() {
                    if let Some(timing) = lyrics::KaraokeTiming::for_text_range(
                        &cue_line.text,
                        cue.byte_start..cue.byte_end_exclusive,
                        cue,
                        *end,
                    ) {
                        highlight_timings[index].push(LyricsHighlightTiming {
                            cue_line: cue_index,
                            target: AndroidLyricsHighlightTarget::Cue {
                                index: cue_part as u32,
                            },
                            timings: vec![timing],
                        });
                    }
                }
                let mut add_timing = |range: std::ops::Range<usize>, target| {
                    let timings: Vec<_> = cues
                        .iter()
                        .filter_map(|(cue, end)| {
                            lyrics::KaraokeTiming::for_text_range(
                                &cue_line.text,
                                range.clone(),
                                cue,
                                *end,
                            )
                        })
                        .collect();
                    if !timings.is_empty() {
                        highlight_timings[index].push(LyricsHighlightTiming {
                            cue_line: cue_index,
                            target,
                            timings,
                        });
                    }
                };
                if readings {
                    let reading = reading_for(
                        &cue_line.text,
                        document.language.as_deref(),
                        supplied,
                        &settings,
                    );
                    if let Some(reading) = reading {
                        if settings.preferences.karaoke_mode {
                            let mut cursor = 0;
                            for (segment_index, segment) in reading.segments.iter().enumerate() {
                                let range = cursor..cursor + segment.surface.len();
                                cursor = range.end;
                                if segment.furigana.is_some() {
                                    add_timing(
                                        range,
                                        AndroidLyricsHighlightTarget::Furigana {
                                            index: segment_index as u32,
                                        },
                                    );
                                }
                            }
                            for (span_index, (range, _)) in
                                reading.romanization_spans.iter().enumerate()
                            {
                                add_timing(
                                    range.clone(),
                                    AndroidLyricsHighlightTarget::Romanization {
                                        index: span_index as u32,
                                    },
                                );
                            }
                        }
                        output.cue_lines[cue_index].reading = Some(reading.into());
                    }
                }
            }
        }
        state.snapshot = Some(Arc::new(AndroidLyricsSnapshot {
            document: Arc::clone(document),
            timing: lyrics::LyricsTiming::new(&document.lines),
            highlight_timings,
        }));
        state.document = Some(projected);
        state
    }

    pub fn default_visualizer_colors(&self, accent: AndroidRgb) -> Vec<AndroidRgb> {
        visualizer_accent_gradient([accent.red, accent.green, accent.blue])
            .into_iter()
            .map(Into::into)
            .collect()
    }

    pub fn release_lyrics_reader(&self) {
        lyrics::release_japanese_reader();
    }

    pub fn visualizer_drawing(
        &self,
        targets: Vec<f64>,
        width: f64,
        height: f64,
        frame_nanos: u64,
        accent: AndroidRgb,
    ) -> AndroidVisualizerDrawing {
        let (fps_limit, appearance) = self.products.settings.visualizer_render_settings();
        let mut state = self
            .drawing
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if targets.is_empty() {
            state.levels.clear();
            state.peaks.clear();
            state.last_frame = None;
        }
        let len = targets.len().max(state.levels.len());
        state.levels.resize(len, 0.0);
        state.peaks.resize(len, VisualizerPeak::default());
        let advance = !targets.is_empty() && state.last_frame != Some(frame_nanos);
        let elapsed_frames = state.last_frame.map_or(1.0, |last| {
            frame_nanos.saturating_sub(last) as f64
                / 1000.0
                / rufin_core::settings::visualizer::VISUALIZER_REFERENCE_FRAME_MICROS
        });
        let (rise, fall) =
            visualizer_smoothing_weights(elapsed_frames, appearance.rise, appearance.fall);
        if advance {
            for index in 0..len {
                let next = targets.get(index).copied().unwrap_or(0.0);
                let value = state.levels[index];
                let level =
                    visualizer_smoothed_level(value, next, if next >= value { rise } else { fall });
                state.levels[index] = level;
                if appearance.peaks {
                    state.peaks[index].advance(
                        level,
                        elapsed_frames / 60.0,
                        appearance.peak_hold,
                        appearance.peak_fall,
                    );
                } else {
                    state.peaks[index] = VisualizerPeak::default();
                }
            }
            state.last_frame = Some(frame_nanos);
        }
        let (columns, cell) = visualizer_column_geometry(width, len, appearance.spacing);
        let (rows, row_height) = visualizer_row_geometry(height, cell, 2.0);
        let peak_levels = state
            .peaks
            .iter()
            .map(|peak| peak.level)
            .collect::<Vec<_>>();
        let colors = appearance
            .colors
            .unwrap_or_else(|| visualizer_accent_gradient([accent.red, accent.green, accent.blue]));
        let color = |value: [f32; 3]| AndroidRgb {
            red: value[0],
            green: value[1],
            blue: value[2],
        };
        AndroidVisualizerDrawing {
            bars: visualizer_bar_levels(&state.levels, columns),
            peaks: if appearance.peaks {
                visualizer_bar_levels(&peak_levels, columns)
            } else {
                Vec::new()
            },
            cell,
            rows: rows as u32,
            row_height,
            gap: appearance.spacing,
            style: serde_json::to_value(appearance.style)
                .expect("Visualizer style serialization")
                .as_str()
                .expect("Visualizer style name")
                .into(),
            opacity: appearance.opacity,
            fps_limit,
            start_color: color(colors[0]),
            end_color: color(colors[1]),
        }
    }
    pub fn artwork_identity(&self, binding: Vec<u8>) -> String {
        artwork::ArtworkBinding::opaque(&binding)
            .stable_identity()
            .to_owned()
    }

    pub fn artwork_preview(&self, identity: Vec<u8>) -> Option<Vec<u8>> {
        self.products
            .artwork
            .preview(
                artwork::ArtworkBinding::opaque(&identity),
                self.products
                    .settings
                    .load()
                    .application
                    .external_metadata_enabled,
            )
            .map(|preview| preview.png.as_ref().clone())
    }

    pub async fn artwork(
        &self,
        identity: Vec<u8>,
        size: u32,
    ) -> Result<Option<Vec<u8>>, AndroidError> {
        let stored = self.products.settings.load();
        let request =
            artwork::ArtworkRequest::new(artwork::ArtworkBinding::opaque(&identity), size, size)
                .with_external(artwork::ExternalPolicy::new(
                    stored.application.external_metadata_enabled,
                    stored.allows_external_metadata_lookup(),
                    stored.lastfm_api_key,
                ));
        let artwork = self.products.artwork.clone();
        let bytes = self
            .products
            .runtime
            .spawn(async move { artwork.image_bytes(request).await })
            .await
            .map_err(error)?
            .map_err(error)?;
        Ok(bytes)
    }

    pub fn subscribe_lyrics(&self) -> Arc<AndroidLyricsSubscription> {
        self.products.lyrics.load_current();
        Arc::new(AndroidLyricsSubscription {
            current: tokio::sync::Mutex::new((self.products.lyrics.current(), true)),
            lyrics: self.products.lyrics.clone(),
        })
    }

    pub fn set_visualizer_enabled(&self, enabled: bool) {
        if !enabled {
            *self
                .drawing
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner()) =
                VisualizerDrawingState::default();
        }
        self.products.playback.set_visualizer_enabled(enabled);
    }

    pub fn subscribe_visualizer(&self) -> Arc<AndroidVisualizerSubscription> {
        Arc::new(AndroidVisualizerSubscription {
            current: tokio::sync::Mutex::new(VisualizerChanges {
                frames: self.visualizer.clone(),
                playback: self.products.playback.updates.subscribe(),
                initial: true,
            }),
            updates: self.products.playback.updates.clone(),
        })
    }
}

#[derive(uniffi::Record)]
pub struct AndroidLyricsLine {
    pub text: String,
    pub start_millis: Option<u64>,
    pub end_millis: Option<u64>,
    pub cue_lines: Vec<AndroidLyricsCueLine>,
    pub pronunciation: Option<String>,
    pub reading: Option<AndroidJapaneseReading>,
}

#[derive(uniffi::Record)]
pub struct AndroidLyricsCueLine {
    pub text: String,
    pub start_millis: Option<u64>,
    pub end_millis: Option<u64>,
    pub agent_id: Option<String>,
    pub cues: Vec<AndroidLyricsCue>,
    pub reading: Option<AndroidJapaneseReading>,
}

#[derive(uniffi::Record)]
pub struct AndroidJapaneseReading {
    pub segments: Vec<lyrics::JapaneseReadingSegment>,
    pub romanization: String,
    pub romanization_spans: Vec<AndroidRomanizationSpan>,
}

#[derive(uniffi::Record)]
pub struct AndroidRomanizationSpan {
    pub start: u32,
    pub end: u32,
}

#[derive(Clone, uniffi::Enum)]
pub enum AndroidLyricsHighlightTarget {
    Cue { index: u32 },
    Furigana { index: u32 },
    Romanization { index: u32 },
}

#[derive(uniffi::Record)]
pub struct AndroidLyricsHighlight {
    pub line: u32,
    pub cue_line: u32,
    pub target: AndroidLyricsHighlightTarget,
    pub progress: f64,
}

#[derive(uniffi::Record)]
pub struct AndroidLyricsFrame {
    pub active_line: Option<u32>,
    pub scroll_line: Option<u32>,
    pub highlight_all: bool,
    pub highlights: Vec<AndroidLyricsHighlight>,
    pub update_after_millis: Option<u64>,
}

impl From<lyrics::JapaneseReading> for AndroidJapaneseReading {
    fn from(reading: lyrics::JapaneseReading) -> Self {
        let mut remaining = reading.romanization.as_str();
        let mut cursor = 0;
        let romanization_spans = reading
            .romanization_spans
            .iter()
            .map(|(_, text)| {
                let (before, after) = remaining
                    .split_once(text.as_str())
                    .expect("Shared romanization text span");
                let start = cursor + before.encode_utf16().count() as u32;
                cursor = start + text.encode_utf16().count() as u32;
                remaining = after;
                AndroidRomanizationSpan { start, end: cursor }
            })
            .collect();
        Self {
            segments: reading.segments,
            romanization: reading.romanization,
            romanization_spans,
        }
    }
}

#[derive(uniffi::Record)]
pub struct AndroidLyricsCue {
    pub text: String,
    pub start_millis: u64,
    pub end_millis: Option<u64>,
    pub utf16_start: Option<u64>,
    pub utf16_end: Option<u64>,
}

#[derive(uniffi::Record)]
pub struct AndroidLyricsDocument {
    pub role: lyrics::LyricsRole,
    pub language: Option<String>,
    pub offset_millis: i64,
    pub lines: Vec<AndroidLyricsLine>,
    pub agents: Vec<lyrics::LyricsAgent>,
}

impl From<&lyrics::LyricsDocument> for AndroidLyricsDocument {
    fn from(document: &lyrics::LyricsDocument) -> Self {
        Self {
            role: document.role,
            language: document.language.clone(),
            offset_millis: document.offset_millis,
            lines: document
                .lines
                .iter()
                .map(|line| AndroidLyricsLine {
                    text: line.text.clone(),
                    start_millis: line.start_millis,
                    end_millis: line.end_millis,
                    pronunciation: None,
                    reading: None,
                    cue_lines: line
                        .cue_lines
                        .iter()
                        .map(|line| AndroidLyricsCueLine {
                            text: line.text.clone(),
                            start_millis: line.start_millis,
                            end_millis: line.end_millis,
                            agent_id: line.agent_id.clone(),
                            reading: None,
                            cues: line
                                .cues
                                .iter()
                                .map(|cue| AndroidLyricsCue {
                                    text: cue.text.clone(),
                                    start_millis: cue.start_millis,
                                    end_millis: cue.end_millis,
                                    utf16_start: line
                                        .text
                                        .get(..cue.byte_start)
                                        .map(|text| text.encode_utf16().count() as u64),
                                    utf16_end: line
                                        .text
                                        .get(..cue.byte_end_exclusive)
                                        .map(|text| text.encode_utf16().count() as u64),
                                })
                                .collect(),
                        })
                        .collect(),
                })
                .collect(),
            agents: document.agents.clone(),
        }
    }
}

#[derive(uniffi::Object)]
pub struct AndroidLyricsSnapshot {
    document: Arc<lyrics::LyricsDocument>,
    timing: lyrics::LyricsTiming,
    highlight_timings: Vec<Vec<LyricsHighlightTiming>>,
}

struct LyricsHighlightTiming {
    cue_line: usize,
    target: AndroidLyricsHighlightTarget,
    timings: Vec<lyrics::KaraokeTiming>,
}

#[uniffi::export]
impl AndroidLyricsSnapshot {
    pub fn frame(
        &self,
        position: u64,
        offset: i64,
        karaoke: bool,
        current_line_only: bool,
        first_line: u32,
        last_line: u32,
    ) -> AndroidLyricsFrame {
        let position = i128::from(position) + i128::from(offset);
        let active = lyrics::active_lyrics_line_index(&self.document.lines, position);
        let range = if current_line_only {
            active.map(|index| index..=index)
        } else {
            Some(first_line as usize..=last_line as usize)
        };
        let mut highlights = Vec::new();
        if karaoke && let Some(range) = range {
            for (line, timings) in self
                .highlight_timings
                .iter()
                .enumerate()
                .take(range.end().saturating_add(1))
                .skip(*range.start())
            {
                for timing in timings {
                    highlights.push(AndroidLyricsHighlight {
                        line: line as u32,
                        cue_line: timing.cue_line as u32,
                        target: timing.target.clone(),
                        progress: timing
                            .timings
                            .iter()
                            .map(|timing| timing.progress(position))
                            .sum::<f64>()
                            .clamp(0.0, 1.0),
                    });
                }
            }
        }
        AndroidLyricsFrame {
            active_line: active.map(|index| index as u32),
            scroll_line: active
                .or_else(|| lyrics::intro_lyrics_line_index(&self.document.lines, position))
                .map(|index| index as u32),
            highlight_all: lyrics::should_highlight_all_lyrics_lines(&self.document.lines),
            highlights,
            update_after_millis: self
                .timing
                .next_after(position, karaoke)
                .and_then(|next| u64::try_from(i128::from(next) - position).ok()),
        }
    }
}

#[derive(uniffi::Record)]
pub struct AndroidLyricsState {
    pub snapshot: Option<Arc<AndroidLyricsSnapshot>>,
    pub current_media_id: Option<playback::CurrentMediaId>,
    pub media_id: Option<playback::CurrentMediaId>,
    pub writable: bool,
    pub clearable: bool,
    pub loading: bool,
    pub instrumental: bool,
    pub document: Option<AndroidLyricsDocument>,
}

#[derive(uniffi::Object)]
pub struct AndroidLyricsSubscription {
    current: tokio::sync::Mutex<(
        tokio::sync::watch::Receiver<rufin_core::lyrics::CurrentLyrics>,
        bool,
    )>,
    lyrics: rufin_core::lyrics::LyricsHandle,
}

#[uniffi::export]
impl AndroidLyricsSubscription {
    pub async fn next(&self) -> Result<Arc<rufin_core::lyrics::CurrentLyrics>, AndroidError> {
        let mut current = self.current.lock().await;
        if current.1 {
            current.1 = false;
        } else {
            current.0.changed().await.map_err(error)?;
        }
        let snapshot = current.0.borrow_and_update().clone();
        self.lyrics.load_current();
        Ok(Arc::new(snapshot))
    }
}

#[derive(uniffi::Object)]
pub struct AndroidVisualizerSubscription {
    current: tokio::sync::Mutex<VisualizerChanges>,
    updates: playback::PlaybackUpdates,
}

struct VisualizerChanges {
    frames: tokio::sync::watch::Receiver<Arc<VisualizerPublication>>,
    playback: playback::PlaybackSubscription,
    initial: bool,
}

#[uniffi::export]
impl AndroidVisualizerSubscription {
    pub async fn next(&self) -> Result<Vec<f64>, AndroidError> {
        let mut current = self.current.lock().await;
        if current.initial {
            current.initial = false;
        } else {
            loop {
                let VisualizerChanges {
                    frames, playback, ..
                } = &mut *current;
                tokio::select! {
                    result = frames.changed() => { result.map_err(error)?; break; },
                    result = playback.recv() => {
                        let view = result.map_err(error)?;
                        if view.as_ref().is_none_or(|projection| projection.view.transport.effective_state() != playback::TransportStatus::Playing
                            || !projection.view.controls.playback_output.is_local()
                            || projection.view.transport.current.as_ref().is_none_or(|media| media.id.run != Some(frames.borrow().run))) {
                            return Ok(Vec::new());
                        }
                    },
                }
            }
        }
        let frame = current.frames.borrow_and_update().clone();
        let active = self
            .updates
            .current()
            .filter(|view| {
                view.transport.effective_state() == playback::TransportStatus::Playing
                    && view.controls.playback_output.is_local()
            })
            .and_then(|view| view.transport.current.clone())
            .is_some_and(|media| media.id.run == Some(frame.run));
        Ok(if active {
            frame.levels.clone()
        } else {
            Vec::new()
        })
    }
}
