//! Current-track waveform selection, caching, and cancellation.
//!
//! Playback supplies the one current-media identity. This owner keeps at most
//! one selected waveform, reuses the one stored for the same audio, and rejects
//! work from a replaced Track or source session. The UI only receives matching peaks.

use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use crate::runtime::WaveformProjection;
use async_channel::Sender;
use audio_processing::generate_waveform_peaks_cancellable;
use playback::{CurrentMedia, CurrentMediaId, StreamRequest};
use tracing::{debug, warn};

use crate::playback::prepare_stream;

#[derive(Clone, Debug, Eq, PartialEq)]
struct WaveformKey {
    media_uri: String,
    duration_millis: i64,
    source_format: Option<String>,
}

impl WaveformKey {
    fn for_media(media: &CurrentMedia) -> Self {
        Self {
            media_uri: media.row.track.media_uri.clone(),
            duration_millis: media.row.track.metadata.duration_millis,
            source_format: media.row.track.metadata.source_format.clone(),
        }
    }
}

struct CurrentWaveform {
    key: WaveformKey,
    media_id: CurrentMediaId,
    request: u64,
    running: bool,
    peaks: Option<Arc<Vec<(f64, f64)>>>,
    task: Option<tokio::task::AbortHandle>,
}

impl Drop for CurrentWaveform {
    fn drop(&mut self) {
        if let Some(task) = self.task.take() {
            task.abort();
        }
    }
}

pub(crate) struct WaveformOwner {
    runtime: tokio::runtime::Handle,
    events: Sender<WaveformProjection>,
    committed: tokio::sync::watch::Receiver<Arc<crate::settings::StoredSettings>>,
    current: Mutex<Option<CurrentWaveform>>,
    next_request: AtomicU64,
}

#[derive(Clone)]
pub(crate) struct WaveformMedia {
    pub(crate) media: Arc<CurrentMedia>,
    pub(crate) source: Arc<crate::source::SourceOwner>,
    pub(crate) catalog: Arc<library::Catalog>,
    pub(crate) request: StreamRequest,
}

impl WaveformOwner {
    pub(crate) fn new(
        runtime: tokio::runtime::Handle,
        events: Sender<WaveformProjection>,
        cache_root: &Path,
        settings: &crate::settings::SettingsFile,
    ) -> Arc<Self> {
        // Waveforms were once JSON files here; the Store holds them now.
        let legacy = cache_root.join("waveforms");
        runtime.spawn_blocking(move || {
            if let Err(error) = std::fs::remove_dir_all(&legacy)
                && error.kind() != std::io::ErrorKind::NotFound
            {
                warn!(%error, "could not remove the old waveform cache");
            }
        });
        Arc::new(Self {
            runtime,
            events,
            committed: settings.changes(),
            current: Mutex::new(None),
            next_request: AtomicU64::new(1),
        })
    }

    fn enabled(&self) -> bool {
        self.committed
            .borrow()
            .ui
            .player_display
            .seekbar_waveform_enabled
    }

    /// The seekbar waveform setting changed: the current track's waveform starts or goes.
    pub(crate) fn settings_changed(self: &Arc<Self>, current: Option<WaveformMedia>) {
        if self.enabled() {
            self.current_changed(current);
        } else {
            self.clear();
        }
    }

    pub(crate) fn current_changed(self: &Arc<Self>, input: Option<WaveformMedia>) {
        let Some(input) = input.filter(|_| self.enabled()) else {
            self.clear();
            return;
        };
        let media = &input.media;
        let key = WaveformKey::for_media(&media);
        let media_id = media.id.clone();
        let mut start = false;
        let request;
        let projection;
        {
            let mut current = self
                .current
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            if let Some(selected) = current.as_mut().filter(|selected| selected.key == key) {
                let replaced_run = selected.media_id != media_id;
                if !replaced_run {
                    return;
                }
                selected.media_id = media_id.clone();
                if !selected.running && selected.peaks.is_none() {
                    selected.request = self.next_request.fetch_add(1, Ordering::AcqRel);
                    selected.running = true;
                    start = true;
                }
                request = selected.request;
                projection = projection_for(selected);
            } else {
                request = self.next_request.fetch_add(1, Ordering::AcqRel);
                let selected = CurrentWaveform {
                    key: key.clone(),
                    media_id: media_id.clone(),
                    request,
                    running: true,
                    peaks: None,
                    task: None,
                };
                projection = projection_for(&selected);
                *current = Some(selected);
                start = true;
            }
        }
        self.publish(projection);
        if !start {
            return;
        }

        self.start_decode(request, key, input);
    }

    fn clear(&self) {
        self.next_request.fetch_add(1, Ordering::AcqRel);
        self.current
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .take();
        self.publish(WaveformProjection::default());
    }

    fn start_decode(self: &Arc<Self>, request: u64, key: WaveformKey, input: WaveformMedia) {
        let owner = Arc::downgrade(self);
        let task_key = key.clone();
        let task = self.runtime.spawn(async move {
            let local = input.source.shared.settings.local_configuration();
            let playable = match crate::loudness::hashed_playable(&input.catalog, input.media.row.track.track_key).await {
                Ok(Some(playable)) => playable,
                result => {
                    if let Err(error) = result {
                        warn!(%error, media_uri = %task_key.media_uri, "failed to read the waveform's track");
                    }
                    if let Some(owner) = owner.upgrade() {
                        owner.finish_failed(request, &task_key);
                    }
                    return;
                }
            };
            let hash = playable.audio_hash;
            if let Some(hash) = hash {
                match input.catalog.waveform(hash).await {
                    Ok(Some(peaks)) => {
                        if let Some(owner) = owner.upgrade() { owner.accept_peaks(request, &task_key, Arc::new(peaks)); }
                        return;
                    }
                    Ok(None) => {}
                    Err(error) => warn!(%error, media_uri = %task_key.media_uri, "failed to read the stored waveform"),
                }
            }
            let catalog = Arc::clone(&input.catalog);
            let downloads = input.source.downloads().clone();
            let stream = match prepare_stream(&playable, input.request, local.as_ref(), Some(&downloads), move |source_id| async move {
                tokio::task::spawn_blocking(move || input.source.client(&source_id)).await.map_err(|error| error.to_string())?
            }).await {
                Ok(stream) => stream,
                Err(error) => {
                    if let Some(owner) = owner.upgrade() {
                        if owner.matches(request, &task_key) {
                            debug!(%error, media_uri = %task_key.media_uri, "waveform source is unavailable");
                        }
                        owner.finish_failed(request, &task_key);
                    }
                    return;
                }
            };
            if !source_and_format_supported(stream.uri(), task_key.source_format.as_deref()) {
                if let Some(owner) = owner.upgrade() {
                    owner.finish_failed(request, &task_key);
                }
                return;
            }
            let cancellation_owner = owner.clone();
            let cancellation_key = task_key.clone();
            let result = tokio::task::spawn_blocking(move || {
                generate_waveform_peaks_cancellable(&stream, || {
                    cancellation_owner
                        .upgrade()
                        .is_none_or(|owner| !owner.matches(request, &cancellation_key))
                })
            })
            .await;
            let Some(owner) = owner.upgrade() else {
                return;
            };
            let peaks = match result {
                Ok(Ok(peaks)) => sanitize_peaks(peaks),
                Ok(Err(error)) => {
                    if owner.matches(request, &task_key) {
                        warn!(%error, media_uri = %task_key.media_uri, "failed to generate waveform");
                    }
                    None
                }
                Err(error) => {
                    if owner.matches(request, &task_key) {
                        warn!(%error, media_uri = %task_key.media_uri, "waveform worker failed");
                    }
                    None
                }
            };
            let Some(peaks) = peaks else {
                owner.finish_failed(request, &task_key);
                return;
            };
            if !owner.matches(request, &task_key) {
                return;
            }
            if let Some(hash) = hash
                && let Err(error) = catalog.put_waveform(hash, &peaks).await
            {
                warn!(%error, media_uri = %task_key.media_uri, "failed to cache waveform");
            }
            owner.accept_peaks(request, &task_key, Arc::new(peaks));
        });
        self.attach_task(request, &key, task.abort_handle());
    }

    fn attach_task(&self, request: u64, key: &WaveformKey, task: tokio::task::AbortHandle) {
        let mut current = self
            .current
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if let Some(selected) = current.as_mut().filter(|selected| {
            selected.running && selected.request == request && &selected.key == key
        }) {
            if let Some(previous) = selected.task.replace(task) {
                previous.abort();
            }
        } else {
            drop(current);
            task.abort();
        }
    }

    fn accept_peaks(&self, request: u64, key: &WaveformKey, peaks: Arc<Vec<(f64, f64)>>) {
        let projection = {
            let mut current = self
                .current
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            let Some(selected) = current
                .as_mut()
                .filter(|selected| selected.request == request && &selected.key == key)
            else {
                return;
            };
            selected.running = false;
            selected.peaks = Some(peaks);
            selected.task = None;
            projection_for(selected)
        };
        self.publish(projection);
    }

    fn finish_failed(&self, request: u64, key: &WaveformKey) {
        let mut current = self
            .current
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if let Some(selected) = current
            .as_mut()
            .filter(|selected| selected.request == request && &selected.key == key)
        {
            selected.running = false;
            selected.task = None;
        }
    }

    fn matches(&self, request: u64, key: &WaveformKey) -> bool {
        self.current
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .as_ref()
            .is_some_and(|selected| selected.request == request && &selected.key == key)
    }

    fn publish(&self, projection: WaveformProjection) {
        let _ = self.events.try_send(projection);
    }
}

fn projection_for(current: &CurrentWaveform) -> WaveformProjection {
    WaveformProjection {
        media_id: Some(current.media_id.clone()),
        peaks: current.peaks.clone(),
    }
}

fn sanitize_peaks(peaks: Vec<(f64, f64)>) -> Option<Vec<(f64, f64)>> {
    let peaks = peaks
        .into_iter()
        .filter(|(left, right)| left.is_finite() && right.is_finite())
        .map(|(left, right)| (left.clamp(0.0, 1.0), right.clamp(0.0, 1.0)))
        .collect::<Vec<_>>();
    (!peaks.is_empty()).then_some(peaks)
}

fn source_and_format_supported(uri: &str, source_format: Option<&str>) -> bool {
    let source_supported =
        uri.starts_with("file://") || uri.starts_with("http://") || uri.starts_with("https://");
    source_supported && !source_format.is_some_and(is_dsd)
}

fn is_dsd(value: &str) -> bool {
    value
        .trim()
        .trim_start_matches('.')
        .to_ascii_lowercase()
        .split(|character: char| !character.is_ascii_alphanumeric())
        .filter(|part| !part.is_empty())
        .any(|part| matches!(part, "dsf" | "dff" | "dsdiff") || part.starts_with("dsd"))
}
