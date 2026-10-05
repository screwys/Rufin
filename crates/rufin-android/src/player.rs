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
pub struct AndroidArtwork {
    pub identity: Vec<u8>,
    pub bytes: Vec<u8>,
}

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
pub struct AndroidPlayerChoice {
    pub id: String,
    pub title: String,
}

#[derive(uniffi::Record)]
pub struct AndroidEqualizerPreset {
    pub id: String,
    pub title: String,
    pub bands: Vec<f64>,
}

#[derive(Clone, uniffi::Record)]
pub struct AndroidFullscreenSettings {
    pub dynamic_background: bool,
    pub background_image: bool,
}

#[derive(uniffi::Record)]
pub struct AndroidPlayerSettings {
    pub private_mode: bool,
    pub equalizer: String,
    pub equalizer_preset: String,
    pub band_titles: Vec<String>,
    pub equalizer_presets: Vec<AndroidEqualizerPreset>,
    pub lyrics: String,
    pub lyrics_providers: Vec<AndroidPlayerChoice>,
    pub lyrics_scroll_pause_millis: u64,
    pub visualizer: String,
    pub visualizer_preset: u32,
    pub visualizer_stale_micros: u64,
    pub visualizer_styles: Vec<AndroidPlayerChoice>,
    pub fullscreen: AndroidFullscreenSettings,
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

#[derive(Clone, uniffi::Record)]
pub struct AndroidLyricsSearchHit {
    pub key: String,
    pub provider: String,
    pub title: String,
    pub artist: String,
    pub album: String,
    pub duration_seconds: u32,
    pub synced: bool,
    pub instrumental: bool,
}

#[derive(Clone, uniffi::Record)]
pub struct AndroidLyricsOperation {
    pub revision: u64,
    pub media_token: Option<String>,
    pub kind: String,
    pub artist: String,
    pub title: String,
    pub results: Vec<AndroidLyricsSearchHit>,
    pub message: Option<String>,
}

#[derive(Clone, Default, uniffi::Record)]
pub struct AndroidLyricsOperations {
    pub search: Option<AndroidLyricsOperation>,
    pub save: Option<AndroidLyricsOperation>,
    pub dictionary: Option<String>,
}

type LyricsSearchResults = Option<(playback::CurrentMediaId, Vec<lyrics::LyricsSearchResult>)>;

#[derive(uniffi::Record)]
pub struct AndroidWaveform {
    pub occurrence_id: Option<String>,
    pub media_run: Option<u64>,
    pub peaks: Vec<f64>,
}

#[derive(Clone, Copy, uniffi::Enum)]
pub enum AndroidOutputKind {
    Local,
    Upnp,
    GoogleCast,
    PlexCompanion,
    Connect,
}

impl From<playback::RemoteOutputProtocol> for AndroidOutputKind {
    fn from(protocol: playback::RemoteOutputProtocol) -> Self {
        match protocol {
            playback::RemoteOutputProtocol::Upnp => Self::Upnp,
            playback::RemoteOutputProtocol::GoogleCast => Self::GoogleCast,
            playback::RemoteOutputProtocol::PlexCompanion => Self::PlexCompanion,
            playback::RemoteOutputProtocol::RufinConnect => Self::Connect,
        }
    }
}

#[derive(uniffi::Record)]
pub struct AndroidOutput {
    pub id: String,
    pub name: String,
    pub kind: AndroidOutputKind,
    pub selected: bool,
    pub available: bool,
    pub has_playback: bool,
}

#[derive(uniffi::Record)]
pub struct AndroidOutputChoices {
    pub outputs: Vec<AndroidOutput>,
    pub discovery_error: Option<String>,
}

#[derive(uniffi::Record)]
pub struct AndroidQueueRow {
    pub occurrence_id: String,
    pub media_uri: String,
    pub title: String,
    pub artist: String,
    pub album: String,
    pub duration_millis: u64,
    pub favorite: bool,
    pub artwork_identity: Option<Vec<u8>>,
}

#[derive(uniffi::Record)]
pub struct AndroidQueuePage {
    pub revision: u64,
    pub total: u64,
    pub window_offset: u64,
    pub window_count: u64,
    pub current_occurrence: Option<String>,
    pub rows: Vec<AndroidQueueRow>,
}

#[derive(uniffi::Object)]
pub struct AndroidPlayer {
    products: ProductHandles,
    visualizer: tokio::sync::watch::Receiver<Arc<VisualizerPublication>>,
    drawing: Mutex<VisualizerDrawingState>,
    lyrics_operations: tokio::sync::watch::Sender<Arc<AndroidLyricsOperations>>,
    search_results: Arc<Mutex<LyricsSearchResults>>,
    waveform: tokio::sync::Mutex<async_channel::Receiver<rufin_core::runtime::WaveformProjection>>,
}

#[uniffi::export]
impl AndroidPlayer {
    pub async fn next_waveform(&self) -> Result<AndroidWaveform, AndroidError> {
        let frame = self.waveform.lock().await.recv().await.map_err(error)?;
        Ok(AndroidWaveform {
            occurrence_id: frame.media_id.as_ref().map(|id| id.occurrence.to_string()),
            media_run: frame
                .media_id
                .as_ref()
                .and_then(|id| id.run.map(playback::RunId::get)),
            peaks: frame
                .peaks
                .as_ref()
                .map(|peaks| {
                    peaks
                        .iter()
                        .map(|(low, high)| low.abs().max(high.abs()))
                        .collect()
                })
                .unwrap_or_default(),
        })
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
        let (lyrics_operations, _) =
            tokio::sync::watch::channel(Arc::new(AndroidLyricsOperations::default()));
        let operations = lyrics_operations.clone();
        let search_results = Arc::new(Mutex::new(None));
        let results = Arc::clone(&search_results);
        products.runtime.spawn(async move {
            use rufin_core::lyrics::LyricsEvent;
            let mut revision = 0;
            while let Ok(event) = lyrics_events.recv().await {
                revision += 1;
                let mut operation = AndroidLyricsOperation {
                    revision,
                    media_token: None,
                    kind: String::new(),
                    artist: String::new(),
                    title: String::new(),
                    results: Vec::new(),
                    message: None,
                };
                match event {
                    LyricsEvent::SearchFinished {
                        media_id,
                        query,
                        result,
                    } => {
                        operation.media_token = Some(media_token(&media_id));
                        operation.artist = query.artist_name;
                        operation.title = query.track_name;
                        operation.kind = "search".into();
                        match result {
                            Ok(found) => {
                                operation.results = found
                                    .iter()
                                    .map(|hit| AndroidLyricsSearchHit {
                                        key: search_result_key(hit),
                                        provider: localization::tr(hit.provider.title()),
                                        title: hit.track_name.clone(),
                                        artist: hit.artist_name.clone(),
                                        album: hit.album_name.clone(),
                                        duration_seconds: hit.duration_seconds,
                                        synced: hit.content.synced_lyrics().is_some(),
                                        instrumental: matches!(
                                            hit.content,
                                            lyrics::LyricsSearchContent::Instrumental
                                        ),
                                    })
                                    .collect();
                                *results
                                    .lock()
                                    .unwrap_or_else(|poisoned| poisoned.into_inner()) =
                                    Some((media_id, found));
                            }
                            Err(message) => operation.message = Some(message),
                        }
                    }
                    LyricsEvent::Saved { media_id, .. } => {
                        operation.media_token = Some(media_token(&media_id));
                        operation.kind = "saved".into();
                        operation.message = Some(localization::tr("Saved"));
                    }
                    LyricsEvent::SourceSaveFailed { media_id, error } => {
                        operation.media_token = Some(media_token(&media_id));
                        operation.kind = "error".into();
                        operation.message = Some(error.to_string());
                    }
                    LyricsEvent::JapaneseDictionaryChanged(status) => {
                        operation.kind = "dictionary".into();
                        operation.message = Some(
                            match status {
                                lyrics::JapaneseDictionaryStatus::Idle => "idle",
                                lyrics::JapaneseDictionaryStatus::Loading => "loading",
                                lyrics::JapaneseDictionaryStatus::Downloading => "downloading",
                                lyrics::JapaneseDictionaryStatus::Ready(_) => "ready",
                                lyrics::JapaneseDictionaryStatus::Failed => "failed",
                            }
                            .into(),
                        );
                    }
                    LyricsEvent::Current(_) => continue,
                }
                let mut state = operations.borrow().as_ref().clone();
                match operation.kind.as_str() {
                    "search" => state.search = Some(operation),
                    "dictionary" => state.dictionary = operation.message,
                    _ => state.save = Some(operation),
                }
                operations.send_replace(Arc::new(state));
            }
        });
        Self {
            products: products.clone(),
            visualizer,
            drawing: Mutex::new(VisualizerDrawingState::default()),
            lyrics_operations,
            search_results,
            waveform: tokio::sync::Mutex::new(waveform),
        }
    }

    async fn select_playback_output(
        &self,
        selected: playback::PlaybackOutput,
    ) -> Result<(), AndroidError> {
        let transport = self.products.playback.transport.clone();
        self.products
            .runtime
            .spawn_blocking(move || {
                transport.select_playback_output(
                    selected,
                    Arc::new(std::sync::atomic::AtomicBool::new(false)),
                )
            })
            .await
            .map_err(error)?
            .map_err(error)
    }

    fn current_media(&self, token: &str) -> Result<playback::CurrentMediaId, AndroidError> {
        let media = self
            .products
            .playback
            .updates
            .current()
            .and_then(|view| view.transport.current.clone())
            .ok_or_else(|| error(localization::tr("Nothing playing")))?;
        if media_token(&media.id) != token {
            return Err(error(localization::tr("This isn't available")));
        }
        Ok(media.id.clone())
    }
}

fn search_result_key(result: &lyrics::LyricsSearchResult) -> String {
    format!("{}:{}", result.provider.key(), result.id)
}

fn reading_for(
    text: &str,
    language: Option<&str>,
    supplied: Option<&str>,
    settings: &lyrics::Settings,
) -> Option<lyrics::JapaneseReading> {
    if !settings.show_furigana && !settings.show_romanization {
        return None;
    }
    let reading = if settings.show_furigana {
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
            settings.show_furigana,
            settings.show_romanization,
        )
    })
}

#[derive(uniffi::Object)]
pub struct AndroidLyricsOperationSubscription {
    current: tokio::sync::Mutex<(
        tokio::sync::watch::Receiver<Arc<AndroidLyricsOperations>>,
        bool,
    )>,
}

#[uniffi::export]
impl AndroidLyricsOperationSubscription {
    pub async fn next(&self) -> Result<AndroidLyricsOperations, AndroidError> {
        let mut current = self.current.lock().await;
        if current.1 {
            current.1 = false;
        } else {
            current.0.changed().await.map_err(error)?;
        }
        let result = current.0.borrow_and_update().as_ref().clone();
        Ok(result)
    }
}

fn media_token(id: &playback::CurrentMediaId) -> String {
    serde_json::json!({"occurrence": id.occurrence.to_string(), "run": id.run.map(playback::RunId::get)}).to_string()
}

#[uniffi::export]
impl AndroidPlayer {
    pub async fn metadata_links(
        &self,
        media_uri: String,
        origin_route: Option<String>,
    ) -> Result<Option<AndroidPlayerMetadata>, AndroidError> {
        let database = self.products.library.clone();
        self.products
            .runtime
            .spawn(async move {
                use rufin_core::route::Route;
                use rufin_core::route::detail_links::{
                    DetailLinks, metadata_links, track_artist_links,
                };
                use rufin_core::settings::layout::LibraryField;
                let origin_route = origin_route
                    .map(|route| serde_json::from_str::<Route>(&route))
                    .transpose()
                    .map_err(error)?;
                let cancellation = library::ReadCancellation::new();
                let entity = library::source_entity_parts(&media_uri);
                let (artists, album, source_id) = match entity {
                    Some((source, kind, _)) if kind == "album" => {
                        let Some((title, artist, credits)) = database
                            .album_metadata_links(&media_uri, &cancellation)
                            .await
                            .map_err(error)?
                        else {
                            return Ok(None);
                        };
                        (
                            metadata_links(
                                LibraryField::AlbumArtist,
                                &artist,
                                Some(&media_uri),
                                &[],
                                &credits,
                            ),
                            metadata_links(
                                LibraryField::Album,
                                &title,
                                Some(&media_uri),
                                &[],
                                &credits,
                            ),
                            source.to_string(),
                        )
                    }
                    Some((source, kind, _)) if kind == "artist" => {
                        let Some(name) = database
                            .artist_name_by_uri(&media_uri, &cancellation)
                            .await
                            .map_err(error)?
                        else {
                            return Ok(None);
                        };
                        let route = match origin_route {
                            Some(Route::AlbumArtistDetail(uri)) if uri == media_uri => {
                                Route::AlbumArtistDetail(uri)
                            }
                            _ => Route::ArtistDetail(media_uri.clone()),
                        };
                        (
                            DetailLinks::route(&name, Some(route)),
                            DetailLinks::default(),
                            source.to_string(),
                        )
                    }
                    _ => {
                        let Some(track) = database
                            .track_row_by_uri(&media_uri, &cancellation)
                            .await
                            .map_err(error)?
                        else {
                            return Ok(None);
                        };
                        (
                            track_artist_links(&track),
                            metadata_links(
                                LibraryField::Album,
                                &track.album,
                                track.album_media_uri.as_deref(),
                                &track.artists,
                                &track.album_artists,
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
                            .collection_artwork_bindings(window, &cancellation)
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
                    source_id: Some(source_id),
                    artist_text: artists.display_text().to_string(),
                    artist_links,
                    album_text: album.display_text().to_string(),
                    album_links,
                }))
            })
            .await
            .map_err(error)?
    }

    pub fn current_lyrics(&self) -> AndroidLyricsState {
        use rufin_core::lyrics::{CurrentLyrics, CurrentLyricsContent};
        let current = self.products.lyrics.current().borrow().clone();
        let mut state = project_lyrics(current.clone(), &self.products.lyrics);
        state.current_media_token = self.products.playback.updates.current().and_then(|view| {
            view.transport
                .current
                .as_ref()
                .map(|media| media_token(&media.id))
        });
        let stored = self.products.settings.load();
        let settings = stored.lyrics;
        if let CurrentLyrics::Ready { origin, .. } = &current {
            if let Some(media) = self
                .products
                .playback
                .updates
                .current()
                .and_then(|view| view.transport.current.clone())
            {
                state.clearable = settings.can_suppress_auto_lyrics(
                    stored.private_mode,
                    &media.media_uri,
                    *origin,
                );
            }
        }
        if !settings.show_furigana && !settings.show_romanization {
            lyrics::release_japanese_reader();
        }
        if let CurrentLyrics::Ready {
            content:
                Some(CurrentLyricsContent::Document {
                    document,
                    pronunciation,
                }),
            ..
        } = current
        {
            let readings = document.is_japanese_for_readings();
            if readings && (settings.show_furigana || settings.show_romanization) {
                if let lyrics::JapaneseDictionaryStatus::Ready(path) =
                    self.products.lyrics.japanese_dictionary(false)
                {
                    lyrics::prepare_japanese_reader(&path);
                }
            }
            if let Some(projected) = &mut state.document {
                let mut reading_timings: Vec<Vec<_>> = if settings.karaoke_mode {
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
                        if readings {
                            let reading = reading_for(
                                &cue_line.text,
                                document.language.as_deref(),
                                supplied,
                                &settings,
                            );
                            if let Some(reading) = reading {
                                if settings.karaoke_mode {
                                    let cues: Vec<_> = cue_line
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
                                        .collect();
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
                                            reading_timings[index].push(ReadingCueTiming {
                                                cue_line: cue_index,
                                                target,
                                                timings,
                                            });
                                        }
                                    };
                                    let mut cursor = 0;
                                    for (segment_index, segment) in
                                        reading.segments.iter().enumerate()
                                    {
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
                    document: Arc::clone(&document),
                    reading_timings,
                }));
            }
        }
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

    pub fn prepare_lyrics_dictionary(&self, retry: bool) {
        self.products.lyrics.japanese_dictionary(retry);
    }

    pub fn seek_from_lyrics(
        &self,
        token: String,
        position: u64,
        offset: i64,
    ) -> Result<(), AndroidError> {
        self.current_media(&token)?;
        self.products.playback.transport.seek_millis(
            lyrics::playback_position_for_lyrics_position(position, offset),
        );
        Ok(())
    }

    pub fn subscribe_lyrics_operations(&self) -> Arc<AndroidLyricsOperationSubscription> {
        Arc::new(AndroidLyricsOperationSubscription {
            current: tokio::sync::Mutex::new((self.lyrics_operations.subscribe(), true)),
        })
    }

    pub fn search_lyrics(
        &self,
        token: String,
        artist: String,
        title: String,
    ) -> Result<(), AndroidError> {
        let id = self.current_media(&token)?;
        *self
            .search_results
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = None;
        let mut state = self.lyrics_operations.borrow().as_ref().clone();
        state.search = Some(AndroidLyricsOperation {
            revision: 0,
            media_token: Some(token),
            kind: "searching".into(),
            artist: artist.clone(),
            title: title.clone(),
            results: Vec::new(),
            message: None,
        });
        self.lyrics_operations.send_replace(Arc::new(state));
        self.products.lyrics.search(
            id,
            lyrics::LyricsQuery {
                artist_name: artist,
                track_name: title,
            },
        );
        Ok(())
    }

    pub fn preview_lyrics_result(&self, token: String, key: String) -> Result<(), AndroidError> {
        let id = self.current_media(&token)?;
        let results = self
            .search_results
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let result = results
            .as_ref()
            .filter(|(media, _)| media == &id)
            .and_then(|(_, results)| {
                results
                    .iter()
                    .find(|result| search_result_key(result) == key)
            })
            .ok_or_else(|| error(localization::tr("This isn't available")))?
            .clone();
        self.products.lyrics.preview(id, result);
        Ok(())
    }

    pub fn save_lyrics_result_to_source(
        &self,
        token: String,
        key: String,
    ) -> Result<(), AndroidError> {
        let id = self.current_media(&token)?;
        let results = self
            .search_results
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let result = results
            .as_ref()
            .filter(|(media, _)| media == &id)
            .and_then(|(_, results)| {
                results
                    .iter()
                    .find(|result| search_result_key(result) == key)
            })
            .ok_or_else(|| error(localization::tr("This isn't available")))?
            .clone();
        self.products.lyrics.save_result_to_source(id, result);
        Ok(())
    }

    pub fn edit_lyrics(&self, token: String, text: String) -> Result<(), AndroidError> {
        self.products
            .lyrics
            .update_lyrics_text(self.current_media(&token)?, text);
        Ok(())
    }

    pub fn apply_lyrics_text_offset(
        &self,
        text: String,
        previous_offset: i64,
        offset: i64,
    ) -> String {
        lyrics::shift_lrc_text_timestamps(&text, offset.saturating_sub(previous_offset))
    }

    pub fn save_lyrics_to_source(&self, token: String, offset: i64) -> Result<(), AndroidError> {
        self.products
            .lyrics
            .save_current_to_source(self.current_media(&token)?, offset);
        Ok(())
    }

    pub async fn clear_fetched_lyrics(
        &self,
        token: String,
    ) -> Result<AndroidPlayerSettings, AndroidError> {
        let media = self
            .products
            .playback
            .updates
            .current()
            .and_then(|view| view.transport.current.clone())
            .ok_or_else(|| error(localization::tr("Nothing playing")))?;
        if media_token(&media.id) != token {
            return Err(error(localization::tr("This isn't available")));
        }
        self.products.lyrics.clear_fetched(media.id.clone());
        let settings = self.products.settings.clone();
        self.products
            .runtime
            .spawn_blocking(move || {
                settings
                    .suppress_fetched_lyrics(media.media_uri.clone())
                    .map_err(error)
            })
            .await
            .map_err(error)??;
        self.settings()
    }

    pub fn lyrics_text(&self, token: String, offset: i64) -> Result<String, AndroidError> {
        let id = self.current_media(&token)?;
        let current = self.products.lyrics.current().borrow().clone();
        if let rufin_core::lyrics::CurrentLyrics::Ready {
            media_id,
            content: Some(rufin_core::lyrics::CurrentLyricsContent::Document { document, .. }),
            ..
        } = current
            && media_id == id
        {
            return Ok(lyrics::lyrics_to_lrc_text(&document, offset));
        }
        Err(error(localization::tr("No lyrics available.")))
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
    pub fn settings(&self) -> Result<AndroidPlayerSettings, AndroidError> {
        let stored = self.products.settings.load();
        Ok(AndroidPlayerSettings {
            private_mode: stored.private_mode,
            equalizer: serde_json::to_string(&stored.playback.equalizer).map_err(error)?,
            equalizer_preset: playback::equalizer_selected_preset(&stored.playback.equalizer),
            band_titles: (0..playback::EQUALIZER_BAND_COUNT)
                .map(playback::equalizer_band_title)
                .collect(),
            equalizer_presets: playback::equalizer_preset_names()
                .into_iter()
                .map(|id| AndroidEqualizerPreset {
                    id: id.into(),
                    title: localization::tr(id),
                    bands: playback::equalizer_preset_bands(id),
                })
                .collect(),
            lyrics: stored.lyrics.user_preferences().to_string(),
            lyrics_providers: lyrics::ExternalLyricsProvider::all()
                .into_iter()
                .map(|provider| AndroidPlayerChoice {
                    id: provider.key().into(),
                    title: localization::tr(provider.title()),
                })
                .collect(),
            lyrics_scroll_pause_millis: lyrics::LYRICS_USER_SCROLL_PAUSE_MS,
            visualizer: serde_json::to_string(&stored.visualizer).map_err(error)?,
            visualizer_preset: stored.visualizer.selected_preset() as u32,
            visualizer_stale_micros: rufin_core::settings::visualizer::VISUALIZER_STALE_MICROS
                as u64,
            visualizer_styles: rufin_core::settings::visualizer::VisualizerStyle::ALL
                .into_iter()
                .map(|style| {
                    let (id, title) = match style {
                        rufin_core::settings::visualizer::VisualizerStyle::Segmented => {
                            ("segmented", "Segmented bars")
                        }
                        rufin_core::settings::visualizer::VisualizerStyle::Solid => {
                            ("solid", "Solid bars")
                        }
                        rufin_core::settings::visualizer::VisualizerStyle::Rounded => {
                            ("rounded", "Rounded bars")
                        }
                        rufin_core::settings::visualizer::VisualizerStyle::Outline => {
                            ("outline", "Outline bars")
                        }
                        rufin_core::settings::visualizer::VisualizerStyle::Line => {
                            ("line", "Spectrum line")
                        }
                        rufin_core::settings::visualizer::VisualizerStyle::Filled => {
                            ("filled", "Filled spectrum")
                        }
                        rufin_core::settings::visualizer::VisualizerStyle::Mirrored => {
                            ("mirrored", "Mirrored bars")
                        }
                        rufin_core::settings::visualizer::VisualizerStyle::Circular => {
                            ("circular", "Circular spectrum")
                        }
                    };
                    AndroidPlayerChoice {
                        id: id.into(),
                        title: localization::tr(title),
                    }
                })
                .collect(),
            fullscreen: AndroidFullscreenSettings {
                dynamic_background: stored.fullscreen_dynamic_background,
                background_image: stored.fullscreen_background_image,
            },
        })
    }

    pub async fn equalizer_enabled(
        &self,
        enabled: bool,
    ) -> Result<AndroidPlayerSettings, AndroidError> {
        let settings = self.products.settings.clone();
        self.products
            .runtime
            .spawn_blocking(move || settings.set_equalizer_enabled(enabled).map_err(error))
            .await
            .map_err(error)??;
        self.settings()
    }

    pub async fn equalizer_preset(
        &self,
        preset: String,
    ) -> Result<AndroidPlayerSettings, AndroidError> {
        let settings = self.products.settings.clone();
        self.products
            .runtime
            .spawn_blocking(move || settings.select_equalizer_preset(preset).map_err(error))
            .await
            .map_err(error)??;
        self.settings()
    }

    pub async fn equalizer_band(
        &self,
        index: u32,
        value: f64,
    ) -> Result<AndroidPlayerSettings, AndroidError> {
        let settings = self.products.settings.clone();
        self.products
            .runtime
            .spawn_blocking(move || {
                settings
                    .set_equalizer_band(index as usize, value)
                    .map_err(error)
            })
            .await
            .map_err(error)??;
        self.settings()
    }

    pub async fn lyrics_preference(
        &self,
        field: String,
        value: String,
    ) -> Result<AndroidPlayerSettings, AndroidError> {
        let value = serde_json::from_str(&value).map_err(error)?;
        let settings = self.products.settings.clone();
        self.products
            .runtime
            .spawn_blocking(move || {
                settings
                    .set_player_lyrics_preference(field, value)
                    .map_err(error)
            })
            .await
            .map_err(error)??;
        self.settings()
    }

    pub async fn visualizer_appearance_field(
        &self,
        field: String,
        value: String,
    ) -> Result<AndroidPlayerSettings, AndroidError> {
        let value = serde_json::from_str(&value).map_err(error)?;
        let settings = self.products.settings.clone();
        self.products
            .runtime
            .spawn_blocking(move || {
                settings
                    .set_visualizer_appearance_field(field, value)
                    .map_err(error)
            })
            .await
            .map_err(error)??;
        self.settings()
    }

    pub async fn visualizer_frame_limit(
        &self,
        value: u32,
    ) -> Result<AndroidPlayerSettings, AndroidError> {
        let settings = self.products.settings.clone();
        self.products
            .runtime
            .spawn_blocking(move || settings.set_visualizer_frame_limit(value).map_err(error))
            .await
            .map_err(error)??;
        self.settings()
    }

    pub async fn visualizer_preset(
        &self,
        slot: u32,
        save: bool,
    ) -> Result<AndroidPlayerSettings, AndroidError> {
        let settings = self.products.settings.clone();
        self.products
            .runtime
            .spawn_blocking(move || {
                settings
                    .update_visualizer_preset(slot as usize, save)
                    .map_err(error)
            })
            .await
            .map_err(error)??;
        self.settings()
    }

    pub fn copy_visualizer_preset(&self) -> String {
        rufin_core::settings::visualizer::VisualizerPreset {
            appearance: self.products.settings.load().visualizer.appearance,
        }
        .to_json()
    }

    pub async fn reset_visualizer_appearance(&self) -> Result<AndroidPlayerSettings, AndroidError> {
        let settings = self.products.settings.clone();
        self.products
            .runtime
            .spawn_blocking(move || {
                settings
                    .set_visualizer_appearance(Default::default())
                    .map_err(error)
            })
            .await
            .map_err(error)??;
        self.settings()
    }

    pub async fn paste_visualizer_preset(
        &self,
        text: String,
    ) -> Result<AndroidPlayerSettings, AndroidError> {
        let preset = rufin_core::settings::visualizer::VisualizerPreset::from_json(&text)
            .ok_or_else(|| error(localization::tr("This isn't available")))?;
        let settings = self.products.settings.clone();
        self.products
            .runtime
            .spawn_blocking(move || {
                settings
                    .set_visualizer_appearance(preset.appearance)
                    .map_err(error)
            })
            .await
            .map_err(error)??;
        self.settings()
    }
    pub async fn outputs(&self) -> Result<AndroidOutputChoices, AndroidError> {
        let transport = self.products.playback.transport.clone();
        let remotes = self
            .products
            .runtime
            .spawn_blocking(move || transport.discover_remote_outputs())
            .await
            .map_err(error)?;
        let (mut remotes, discovery_error) = match remotes {
            Ok(remotes) => (remotes, None),
            Err(error) => (Vec::new(), Some(error)),
        };
        let selected = self.products.playback.transport.playback_output();
        let connect = self.products.connect.status();
        if let playback::PlaybackOutput::Remote(remote) = &selected
            && !remotes
                .iter()
                .any(|candidate| candidate.id == remote.id && candidate.protocol == remote.protocol)
        {
            remotes.push(remote.clone());
        }
        let mut outputs = vec![AndroidOutput {
            id: String::new(),
            name: localization::tr("This device"),
            kind: AndroidOutputKind::Local,
            selected: selected.is_local(),
            available: true,
            has_playback: false,
        }];
        outputs.extend(remotes.into_iter().map(|remote| {
            let device = (remote.protocol == playback::RemoteOutputProtocol::RufinConnect)
                .then(|| connect.devices.iter().find(|device| device.id == remote.id))
                .flatten();
            AndroidOutput {
                selected: matches!(&selected, playback::PlaybackOutput::Remote(current)
                if current.id == remote.id && current.protocol == remote.protocol),
                kind: remote.protocol.into(),
                id: remote.id,
                name: remote.name,
                available: remote.protocol != playback::RemoteOutputProtocol::RufinConnect
                    || (connect.settings.enabled
                        && device.is_some_and(|device| device.enrolled && device.reachable)),
                has_playback: device.is_some_and(|device| device.has_playback),
            }
        }));
        Ok(AndroidOutputChoices {
            outputs,
            discovery_error,
        })
    }

    pub async fn select_output(&self, output: AndroidOutput) -> Result<(), AndroidError> {
        let protocol = match output.kind {
            AndroidOutputKind::Local => None,
            AndroidOutputKind::Upnp => Some(playback::RemoteOutputProtocol::Upnp),
            AndroidOutputKind::GoogleCast => Some(playback::RemoteOutputProtocol::GoogleCast),
            AndroidOutputKind::PlexCompanion => Some(playback::RemoteOutputProtocol::PlexCompanion),
            AndroidOutputKind::Connect => Some(playback::RemoteOutputProtocol::RufinConnect),
        };
        let selected = match protocol {
            None => playback::PlaybackOutput::Local,
            Some(protocol) => playback::PlaybackOutput::Remote(playback::RemoteOutput {
                id: output.id,
                name: output.name,
                protocol,
            }),
        };
        self.select_playback_output(selected).await
    }

    pub async fn select_local_output(&self) -> Result<(), AndroidError> {
        self.select_playback_output(playback::PlaybackOutput::Local)
            .await
    }

    pub async fn artwork(
        &self,
        identity: Vec<u8>,
        size: u32,
    ) -> Result<Option<AndroidArtwork>, AndroidError> {
        let stored = self.products.settings.load();
        let request =
            artwork::ArtworkRequest::new(artwork::ArtworkBinding::opaque(&identity), size, size)
                .with_external(artwork::ExternalPolicy::new(
                    stored.external_metadata_enabled,
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
        Ok(bytes.map(|bytes| AndroidArtwork { identity, bytes }))
    }

    pub async fn set_favorite(
        &self,
        media_uri: String,
        favorite: bool,
    ) -> Result<bool, AndroidError> {
        let current = self.products.playback.updates.current();
        let current = current
            .as_ref()
            .and_then(|view| view.transport.current.as_ref());
        tracing::info!(
            favorite,
            run = current.and_then(|current| current.id.run.map(playback::RunId::get)),
            current_target = current.is_some_and(|current| current.media_uri == media_uri),
            "Android player favorite requested"
        );
        self.products
            .source
            .set_favorite_with_result(library::FavoriteTarget::Track(media_uri), favorite)
            .recv()
            .await
            .map_err(error)?
            .map_err(error)
    }

    pub async fn queue_page(
        &self,
        offset: u64,
        limit: u32,
        filter: String,
    ) -> Result<AndroidQueuePage, AndroidError> {
        let view = self.products.playback.updates.current();
        let Some(view) = view else {
            return Ok(AndroidQueuePage {
                revision: 0,
                total: 0,
                window_offset: 0,
                window_count: 0,
                current_occurrence: None,
                rows: Vec::new(),
            });
        };
        let database = self.products.library.clone();
        let queue = self.products.playback.queue.clone();
        let (rows, total, window_offset) = self
            .products
            .runtime
            .spawn(async move {
                let page = tokio::task::spawn_blocking(move || queue.page(offset, limit, filter))
                    .await
                    .map_err(error)?
                    .map_err(error)?;
                Ok::<_, AndroidError>((
                    database
                        .prepared_queue_page(&page.rows)
                        .await
                        .map_err(error)?,
                    page.total,
                    page.window_offset,
                ))
            })
            .await
            .map_err(error)?
            .map_err(error)?;
        Ok(AndroidQueuePage {
            revision: view.queue.revision,
            total: view.queue.total as u64,
            window_offset: window_offset as u64,
            window_count: total,
            current_occurrence: view
                .queue
                .current_occurrence
                .as_ref()
                .map(ToString::to_string),
            rows: rows
                .into_iter()
                .map(|row| AndroidQueueRow {
                    occurrence_id: row.occurrence.to_string(),
                    media_uri: row.media_uri.clone(),
                    title: row.title.clone(),
                    artist: row.artist.clone(),
                    album: row.album.clone(),
                    duration_millis: row.duration_millis.max(0) as u64,
                    favorite: row.favorite,
                    artwork_identity: row.artwork_binding.clone(),
                })
                .collect(),
        })
    }

    pub fn activate_queue(&self, occurrence_id: String) {
        self.products
            .playback
            .queue
            .activate(library::OccurrenceId::new(occurrence_id));
    }

    pub fn remove_queue(&self, occurrence_id: String) {
        self.products
            .playback
            .queue
            .remove(library::OccurrenceId::new(occurrence_id));
    }

    pub fn move_queue(
        &self,
        occurrence_id: String,
        target_occurrence: Option<String>,
        after: bool,
    ) {
        self.products
            .playback
            .queue
            .reorder(playback::QueueReorderRequest {
                occurrences: vec![library::OccurrenceId::new(occurrence_id)],
                target: target_occurrence.map_or(library::QueueReorderTarget::End, |id| {
                    if after {
                        library::QueueReorderTarget::After(library::OccurrenceId::new(id))
                    } else {
                        library::QueueReorderTarget::Before(library::OccurrenceId::new(id))
                    }
                }),
            });
    }

    pub fn clear_queue(&self, include_current: bool) {
        self.products.playback.queue.clear(include_current);
    }

    pub async fn create_queue_playlist(
        &self,
        name: String,
        use_current_source: bool,
        public: Option<bool>,
    ) -> Result<(), AndroidError> {
        let queue = self.products.playback.queue.clone();
        let uris = self
            .products
            .runtime
            .spawn_blocking(move || queue.media_uris())
            .await
            .map_err(error)?
            .map_err(error)?;
        let source = self
            .products
            .source
            .list_sources()
            .selected_source_id
            .filter(|_| use_current_source);
        let created = rufin_core::playlists::create_playlist(
            &self.products.source,
            source.clone(),
            name,
            uris,
            public,
        )
        .recv()
        .await
        .map_err(error)?
        .map_err(error)?;
        if let Some(id) = created {
            self.products
                .settings
                .remember_created_playlist(source, id, Some(use_current_source))
                .map_err(error)?;
        }
        Ok(())
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
        self.products
            .playback
            .transport
            .set_visualizer_enabled(enabled);
    }

    pub fn toggle_auto_dj(&self) {
        self.products.playback.transport.toggle_auto_dj();
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
pub struct AndroidReadingSegment {
    pub surface: String,
    pub furigana: Option<String>,
}

#[derive(uniffi::Record)]
pub struct AndroidJapaneseReading {
    pub segments: Vec<AndroidReadingSegment>,
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
pub struct AndroidLyricsPosition {
    pub active_line: Option<u32>,
    pub scroll_line: Option<u32>,
    pub highlight_all: bool,
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
            segments: reading
                .segments
                .into_iter()
                .map(|segment| AndroidReadingSegment {
                    surface: segment.surface,
                    furigana: segment.furigana,
                })
                .collect(),
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
    pub byte_start: u64,
    pub byte_end_exclusive: u64,
    pub utf16_start: Option<u64>,
    pub utf16_end: Option<u64>,
}

#[derive(uniffi::Record)]
pub struct AndroidLyricsAgent {
    pub id: String,
    pub role: String,
    pub name: Option<String>,
}

#[derive(uniffi::Record)]
pub struct AndroidLyricsDocument {
    pub role: String,
    pub language: Option<String>,
    pub offset_millis: i64,
    pub lines: Vec<AndroidLyricsLine>,
    pub agents: Vec<AndroidLyricsAgent>,
}

impl From<&lyrics::LyricsDocument> for AndroidLyricsDocument {
    fn from(document: &lyrics::LyricsDocument) -> Self {
        Self {
            role: document.role.key().to_string(),
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
                                    byte_start: cue.byte_start as u64,
                                    byte_end_exclusive: cue.byte_end_exclusive as u64,
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
            agents: document
                .agents
                .iter()
                .map(|agent| AndroidLyricsAgent {
                    id: agent.id.clone(),
                    name: agent.name.clone(),
                    role: match agent.role {
                        lyrics::LyricsAgentRole::Main => "main",
                        lyrics::LyricsAgentRole::Voice => "voice",
                        lyrics::LyricsAgentRole::Background => "background",
                        lyrics::LyricsAgentRole::Group => "group",
                    }
                    .to_string(),
                })
                .collect(),
        }
    }
}

#[derive(uniffi::Object)]
pub struct AndroidLyricsSnapshot {
    document: Arc<lyrics::LyricsDocument>,
    reading_timings: Vec<Vec<ReadingCueTiming>>,
}

struct ReadingCueTiming {
    cue_line: usize,
    target: AndroidLyricsHighlightTarget,
    timings: Vec<lyrics::KaraokeTiming>,
}

#[uniffi::export]
impl AndroidLyricsSnapshot {
    pub fn position(&self, position: u64, offset: i64) -> AndroidLyricsPosition {
        let position = i128::from(position) + i128::from(offset);
        let active = lyrics::active_lyrics_line_index(&self.document.lines, position);
        AndroidLyricsPosition {
            active_line: active.map(|index| index as u32),
            scroll_line: active
                .or_else(|| lyrics::intro_lyrics_line_index(&self.document.lines, position))
                .map(|index| index as u32),
            highlight_all: lyrics::should_highlight_all_lyrics_lines(&self.document.lines),
        }
    }

    pub fn highlights(
        &self,
        position: u64,
        offset: i64,
        first_line: u32,
        last_line: u32,
    ) -> Vec<AndroidLyricsHighlight> {
        let position = i128::from(position) + i128::from(offset);
        let mut output = Vec::new();
        for (line_index, line) in self
            .document
            .lines
            .iter()
            .enumerate()
            .take(last_line as usize + 1)
            .skip(first_line as usize)
        {
            for (cue_line_index, cue_line) in line.cue_lines.iter().enumerate() {
                for (cue_index, cue) in cue_line.cues.iter().enumerate() {
                    if let Some(timing) = lyrics::KaraokeTiming::for_text_range(
                        &cue_line.text,
                        cue.byte_start..cue.byte_end_exclusive,
                        cue,
                        lyrics::effective_cue_end(
                            &self.document.lines,
                            line_index,
                            cue_line,
                            cue_index,
                        ),
                    ) {
                        output.push(AndroidLyricsHighlight {
                            line: line_index as u32,
                            cue_line: cue_line_index as u32,
                            target: AndroidLyricsHighlightTarget::Cue {
                                index: cue_index as u32,
                            },
                            progress: timing.progress(position),
                        });
                    }
                }
            }
        }
        for (line, timings) in self
            .reading_timings
            .iter()
            .enumerate()
            .take(last_line as usize + 1)
            .skip(first_line as usize)
        {
            for timing in timings {
                output.push(AndroidLyricsHighlight {
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
        output
    }
}

#[derive(uniffi::Record)]
pub struct AndroidLyricsState {
    pub snapshot: Option<Arc<AndroidLyricsSnapshot>>,
    pub current_media_token: Option<String>,
    pub occurrence_id: Option<String>,
    pub media_run: Option<u64>,
    pub media_token: Option<String>,
    pub writable: bool,
    pub clearable: bool,
    pub loading: bool,
    pub instrumental: bool,
    pub document: Option<AndroidLyricsDocument>,
    pub pronunciation: Option<AndroidLyricsDocument>,
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
    pub async fn next(&self) -> Result<AndroidLyricsState, AndroidError> {
        let mut current = self.current.lock().await;
        if current.1 {
            current.1 = false;
        } else {
            current.0.changed().await.map_err(error)?;
        }
        let lyrics = current.0.borrow_and_update().clone();
        self.lyrics.load_current();
        let state = project_lyrics(lyrics, &self.lyrics);
        Ok(state)
    }
}

fn project_lyrics(
    current: rufin_core::lyrics::CurrentLyrics,
    handle: &rufin_core::lyrics::LyricsHandle,
) -> AndroidLyricsState {
    use rufin_core::lyrics::{CurrentLyrics, CurrentLyricsContent};
    let lyrics = current;
    let mut state = AndroidLyricsState {
        snapshot: None,
        current_media_token: None,
        occurrence_id: None,
        media_run: None,
        media_token: None,
        writable: false,
        clearable: false,
        loading: false,
        instrumental: false,
        document: None,
        pronunciation: None,
    };
    match lyrics {
        CurrentLyrics::Cleared => {}
        CurrentLyrics::Loading { media_id } => {
            state.media_run = media_id.run.map(playback::RunId::get);
            state.media_token = Some(media_token(&media_id));
            state.occurrence_id = Some(media_id.occurrence.to_string());
            state.loading = true;
        }
        CurrentLyrics::Ready {
            media_id, content, ..
        } => {
            state.media_run = media_id.run.map(playback::RunId::get);
            state.media_token = Some(media_token(&media_id));
            state.writable = handle.current_writable(&media_id);
            state.occurrence_id = Some(media_id.occurrence.to_string());
            match content {
                Some(CurrentLyricsContent::Instrumental) => state.instrumental = true,
                Some(CurrentLyricsContent::Document {
                    document,
                    pronunciation,
                }) => {
                    state.snapshot = Some(Arc::new(AndroidLyricsSnapshot {
                        document: Arc::clone(&document),
                        reading_timings: Vec::new(),
                    }));
                    state.document = Some(document.as_ref().into());
                    state.pronunciation = pronunciation.as_deref().map(Into::into);
                }
                None => {}
            }
        }
    }
    state
}

#[derive(uniffi::Record)]
pub struct AndroidVisualizerFrame {
    pub levels: Vec<f64>,
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
    pub async fn next(&self) -> Result<AndroidVisualizerFrame, AndroidError> {
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
                            return Ok(AndroidVisualizerFrame { levels: Vec::new() });
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
        Ok(AndroidVisualizerFrame {
            levels: if active {
                frame.levels.clone()
            } else {
                Vec::new()
            },
        })
    }
}
