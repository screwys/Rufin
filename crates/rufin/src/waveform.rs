//! Current-track waveform selection, caching, and cancellation.
//!
//! Playback supplies the one current-media identity. This owner keeps at most
//! one selected waveform, reuses the disk cache, and rejects work from a
//! replaced Track or source session. The UI only receives matching peaks.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use async_channel::Sender;
use library::{SourceKey, TrackKey};
use playback::{CurrentMedia, CurrentMediaId, SourceSessionEpoch, StreamRequest};
use playback_gstreamer::generate_waveform_peaks_cancellable;
use serde::{Deserialize, Serialize};
use sources::Source;
use tracing::{debug, warn};
use ui::runtime::WaveformProjection;

use crate::playback::prepare_stream;

const CACHE_VERSION: u8 = 1;
const CACHE_DIRECTORY: &str = "waveforms";

#[derive(Clone, Debug, Eq, PartialEq)]
struct WaveformKey {
    source_key: SourceKey,
    source_session_epoch: SourceSessionEpoch,
    track_key: TrackKey,
    duration_millis: i64,
    media_uri: Option<String>,
    source_format: Option<String>,
    cue_window: Option<(u64, u64)>,
}

impl WaveformKey {
    fn for_media(media: &CurrentMedia) -> Self {
        Self {
            source_key: media.id.source_key,
            source_session_epoch: media.id.source_session_epoch,
            track_key: media
                .track
                .track_key
                .expect("current media has a Track key"),
            duration_millis: media.track.duration_millis,
            media_uri: media.track.media_uri.clone(),
            source_format: media.track.source_format.clone(),
            cue_window: media
                .track
                .cue_start_millis
                .zip(media.track.cue_end_millis)
                .and_then(|(start, end)| {
                    Some((u64::try_from(start).ok()?, u64::try_from(end).ok()?))
                }),
        }
    }

    fn cache_path(&self, root: &Path) -> PathBuf {
        let identity = format!(
            "{}\n{}\n{}\n{:?}\n{:?}\n{:?}",
            self.track_key,
            self.duration_millis,
            self.media_uri.as_deref().unwrap_or_default(),
            self.source_format,
            self.cue_window,
            CACHE_VERSION,
        );
        root.join(CACHE_DIRECTORY)
            .join(self.source_key.to_string())
            .join(format!("{:x}.json", md5::compute(identity)))
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

#[derive(Deserialize, Serialize)]
struct CachedWaveform {
    version: u8,
    duration_millis: i64,
    peaks: Vec<(f64, f64)>,
}

pub(crate) struct WaveformOwner {
    runtime: tokio::runtime::Handle,
    events: Sender<WaveformProjection>,
    cache_root: PathBuf,
    enabled: AtomicBool,
    current: Mutex<Option<CurrentWaveform>>,
    next_request: AtomicU64,
}

#[derive(Clone)]
pub(crate) struct WaveformMedia {
    pub(crate) media: Arc<CurrentMedia>,
    pub(crate) source: Option<Arc<Source>>,
    pub(crate) request: StreamRequest,
}

impl WaveformOwner {
    pub(crate) fn new(
        runtime: tokio::runtime::Handle,
        events: Sender<WaveformProjection>,
        cache_root: PathBuf,
        enabled: bool,
    ) -> Arc<Self> {
        Arc::new(Self {
            runtime,
            events,
            cache_root,
            enabled: AtomicBool::new(enabled),
            current: Mutex::new(None),
            next_request: AtomicU64::new(1),
        })
    }

    pub(crate) fn settings_changed(
        self: &Arc<Self>,
        enabled: bool,
        current: Option<WaveformMedia>,
    ) {
        if self.enabled.swap(enabled, Ordering::AcqRel) == enabled {
            return;
        }
        if enabled {
            self.current_changed(current);
        } else {
            self.clear();
        }
    }

    pub(crate) fn current_changed(self: &Arc<Self>, input: Option<WaveformMedia>) {
        let Some(input) = input.filter(|input| {
            self.enabled.load(Ordering::Acquire) && input.media.track.duration_millis > 0
        }) else {
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

        if let Some(peaks) = load_cached(&self.cache_root, &key) {
            self.accept_peaks(request, &key, Arc::new(peaks));
            return;
        }
        self.start_decode(request, key, input);
    }

    pub(crate) fn remove_source_cache(&self, source: SourceKey) -> std::io::Result<()> {
        if self
            .current
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .as_ref()
            .is_some_and(|current| current.key.source_key == source)
        {
            self.clear();
        }
        let directory = self
            .cache_root
            .join(CACHE_DIRECTORY)
            .join(source.to_string());
        match fs::remove_dir_all(directory) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(error),
        }
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
            let stream = match prepare_stream(input.source, input.request).await
            {
                Ok(stream) => stream,
                Err(error) => {
                    if let Some(owner) = owner.upgrade() {
                        if owner.matches(request, &task_key) {
                            debug!(%error, track_key = %task_key.track_key, "waveform source is unavailable");
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
                        warn!(%error, track_key = %task_key.track_key, "failed to generate waveform");
                    }
                    None
                }
                Err(error) => {
                    if owner.matches(request, &task_key) {
                        warn!(%error, track_key = %task_key.track_key, "waveform worker failed");
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
            if let Err(error) = save_cached(&owner.cache_root, &task_key, &peaks) {
                warn!(%error, track_key = %task_key.track_key, "failed to cache waveform");
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

fn load_cached(root: &Path, key: &WaveformKey) -> Option<Vec<(f64, f64)>> {
    let value = fs::read_to_string(key.cache_path(root)).ok()?;
    let cached = serde_json::from_str::<CachedWaveform>(&value).ok()?;
    if cached.version != CACHE_VERSION || cached.duration_millis != key.duration_millis {
        return None;
    }
    sanitize_peaks(cached.peaks)
}

fn save_cached(root: &Path, key: &WaveformKey, peaks: &[(f64, f64)]) -> Result<(), String> {
    let path = key.cache_path(root);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|error| error.to_string())?;
    }
    let value = serde_json::to_vec(&CachedWaveform {
        version: CACHE_VERSION,
        duration_millis: key.duration_millis,
        peaks: peaks.to_vec(),
    })
    .map_err(|error| error.to_string())?;
    fs::write(path, value).map_err(|error| error.to_string())
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

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn removing_one_source_cache_preserves_other_waveforms() {
        let directory = tempfile::tempdir().expect("Waveform cache directory");
        let first = directory.path().join(CACHE_DIRECTORY).join("1");
        let second = directory.path().join(CACHE_DIRECTORY).join("2");
        fs::create_dir_all(&first).expect("first source cache");
        fs::create_dir_all(&second).expect("second source cache");
        fs::write(first.join("track.json"), b"waveform").expect("first waveform");
        fs::write(second.join("track.json"), b"waveform").expect("second waveform");
        let (events, _) = async_channel::unbounded();
        let owner = WaveformOwner::new(
            tokio::runtime::Handle::current(),
            events,
            directory.path().to_path_buf(),
            true,
        );

        owner
            .remove_source_cache(SourceKey::from_raw(1))
            .expect("remove first source Waveforms");

        assert!(!first.exists());
        assert!(second.exists());
    }
}
