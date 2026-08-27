//! Sequential bounded loudness analysis for one missing Track or Album.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use library::{
    AlbumLoudnessTrack, LoudnessMeasurement, ReadCancellation, SourceKey, TrackLoudnessWork,
};
use playback::{
    LoudnessNormalization, LoudnessNormalizationScope, SourceSessionEpoch, StreamQuality,
    StreamRequest,
};
use playback_gstreamer::{LoudnessAnalysis, album_loudness, analyze_loudness_cancellable};
use tracing::{info, warn};

use crate::playback::prepare_stream;
use crate::source::{ActiveSource, SelectedSourceState, WeakActiveSource};

struct ActiveAnalysis {
    source_key: SourceKey,
    epoch: SourceSessionEpoch,
    selected: WeakActiveSource,
    cancelled: Arc<AtomicBool>,
    task: Option<tokio::task::AbortHandle>,
    restart: bool,
    scope: LoudnessNormalizationScope,
    write_tags: bool,
}

impl ActiveAnalysis {
    fn cancel(mut self) {
        self.cancelled.store(true, Ordering::Release);
        if let Some(task) = self.task.take() {
            task.abort();
        }
    }
}

pub(crate) struct LoudnessAnalysisOwner {
    runtime: tokio::runtime::Handle,
    active: Mutex<Option<ActiveAnalysis>>,
    blocked: Mutex<AnalysisFailureBlock>,
}

#[derive(Default)]
struct AnalysisFailureBlock(Option<(SourceKey, SourceSessionEpoch)>);

impl AnalysisFailureBlock {
    fn blocks(&self, source: SourceKey, epoch: SourceSessionEpoch) -> bool {
        self.0 == Some((source, epoch))
    }

    fn record(&mut self, source: SourceKey, epoch: SourceSessionEpoch) {
        self.0 = Some((source, epoch));
    }

    fn clear(&mut self) {
        self.0 = None;
    }
}

impl LoudnessAnalysisOwner {
    pub(crate) fn new(runtime: tokio::runtime::Handle) -> Arc<Self> {
        Arc::new(Self {
            runtime,
            active: Mutex::new(None),
            blocked: Mutex::new(AnalysisFailureBlock::default()),
        })
    }

    pub(crate) fn settings_changed(
        self: &Arc<Self>,
        normalization: LoudnessNormalization,
        scope: LoudnessNormalizationScope,
        write_tags: bool,
        selected: Option<Arc<ActiveSource>>,
    ) {
        if normalization != LoudnessNormalization::EbuR128 {
            self.cancel();
            return;
        }
        let Some(selected) = selected else {
            self.cancel();
            return;
        };
        let Some(state) = selected.resolve() else {
            self.cancel();
            return;
        };
        let source_key = state.source_key;
        let epoch = state.source_session_epoch;
        if self
            .blocked
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .blocks(source_key, epoch)
        {
            return;
        }
        let cancelled = Arc::new(AtomicBool::new(false));
        {
            let mut active = self.active.lock().unwrap_or_else(|p| p.into_inner());
            if active.as_ref().is_some_and(|current| {
                current.source_key == source_key
                    && current.epoch == epoch
                    && current.scope == scope
                    && current.write_tags == write_tags
            }) {
                return;
            }
            if let Some(previous) = active.take() {
                previous.cancel();
            }
            *active = Some(ActiveAnalysis {
                source_key,
                epoch,
                selected: selected.downgrade(),
                cancelled: Arc::clone(&cancelled),
                task: None,
                restart: false,
                scope,
                write_tags,
            });
        }
        let owner = Arc::downgrade(self);
        let weak = selected.downgrade();
        let task_cancelled = Arc::clone(&cancelled);
        let task = self.runtime.spawn(async move {
            info!(%source_key, "analyzing missing loudness data");
            let failed =
                analyze_selected(weak, scope, write_tags, Arc::clone(&task_cancelled)).await;
            if let Some(owner) = owner.upgrade() {
                owner.finish(&task_cancelled, normalization, scope, write_tags, failed);
            }
        });
        let abort = task.abort_handle();
        let mut active = self.active.lock().unwrap_or_else(|p| p.into_inner());
        if let Some(current) = active
            .as_mut()
            .filter(|current| Arc::ptr_eq(&current.cancelled, &cancelled))
        {
            current.task = Some(abort);
        } else {
            abort.abort();
        }
    }

    pub(crate) fn library_changed(
        self: &Arc<Self>,
        normalization: LoudnessNormalization,
        scope: LoudnessNormalizationScope,
        write_tags: bool,
        selected: Option<Arc<ActiveSource>>,
    ) {
        let Some(selected) = selected else {
            self.cancel();
            return;
        };
        let Some(state) = selected.resolve() else {
            self.cancel();
            return;
        };
        self.blocked
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clear();
        let mut active = self.active.lock().unwrap_or_else(|p| p.into_inner());
        if let Some(current) = active.as_mut().filter(|current| {
            current.source_key == state.source_key && current.epoch == state.source_session_epoch
        }) {
            current.restart = true;
            return;
        }
        drop(active);
        drop(state);
        self.settings_changed(normalization, scope, write_tags, Some(selected));
    }

    pub(crate) fn cancel(&self) {
        if let Some(active) = self.active.lock().unwrap_or_else(|p| p.into_inner()).take() {
            active.cancel();
        }
    }

    fn finish(
        self: &Arc<Self>,
        cancelled: &Arc<AtomicBool>,
        normalization: LoudnessNormalization,
        scope: LoudnessNormalizationScope,
        write_tags: bool,
        failed: bool,
    ) {
        let mut active = self.active.lock().unwrap_or_else(|p| p.into_inner());
        let completed = active
            .as_ref()
            .is_some_and(|current| Arc::ptr_eq(&current.cancelled, cancelled))
            .then(|| active.take())
            .flatten();
        let restart = completed.as_ref().and_then(|current| {
            current
                .restart
                .then(|| current.selected.upgrade())
                .flatten()
        });
        if failed && let Some(current) = completed.as_ref() {
            self.blocked
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .record(current.source_key, current.epoch);
        }
        drop(active);
        if !failed && let Some(selected) = restart {
            self.settings_changed(normalization, scope, write_tags, Some(selected));
        }
    }
}

async fn analyze_selected(
    selected: WeakActiveSource,
    scope: LoudnessNormalizationScope,
    write_tags: bool,
    cancelled: Arc<AtomicBool>,
) -> bool {
    if write_tags {
        let Some(state) = current(&selected, &cancelled) else {
            return false;
        };
        if state.configuration.kind == sources::LOCAL_SOURCE_ID
            && let Some(source) = state.source.as_ref()
            && let Err(error) = source
                .backfill_local_r128_tags(&state.database, state.source_key)
                .await
        {
            warn!(%error, "could not write stored EBU R128 tags");
        }
    }
    loop {
        let Some(state) = current(&selected, &cancelled) else {
            return false;
        };
        if scope == LoudnessNormalizationScope::Album {
            match state
                .database
                .next_missing_album_loudness(state.source_key, &ReadCancellation::new())
                .await
            {
                Ok(Some(work)) => {
                    let mut analyses = Vec::with_capacity(work.tracks.len());
                    let mut track_tags = Vec::with_capacity(work.tracks.len());
                    for track in &work.tracks {
                        match analyze_album_track(&state, track, &cancelled).await {
                            Ok(analysis) => {
                                let value = analysis.measurement();
                                let measurement = LoudnessMeasurement {
                                    analysis_key: track.expected_analysis_key,
                                    integrated_lufs: value.integrated_lufs,
                                    true_peak: value.true_peak,
                                    replay_gain_db: None,
                                    replay_gain_peak: None,
                                };
                                if let Err(error) = state
                                    .database
                                    .write_track_analyzed_loudness(
                                        state.source_key,
                                        track.track_key,
                                        &measurement,
                                    )
                                    .await
                                {
                                    warn!(%error, "could not store Track loudness");
                                    return true;
                                }
                                track_tags.push((track.track_key, value.integrated_lufs));
                                analyses.push(analysis);
                            }
                            Err(error) => {
                                warn!(%error, track_key=%track.track_key, "could not analyze Track loudness");
                                analyses.clear();
                                return true;
                            }
                        }
                    }
                    if !analyses.is_empty() {
                        match album_loudness(&analyses) {
                            Ok(value) => {
                                let measurement = LoudnessMeasurement {
                                    analysis_key: work.expected_analysis_key,
                                    integrated_lufs: value.integrated_lufs,
                                    true_peak: value.true_peak,
                                    replay_gain_db: None,
                                    replay_gain_peak: None,
                                };
                                if let Err(error) = state
                                    .database
                                    .write_album_analyzed_loudness(
                                        state.source_key,
                                        work.album_key,
                                        &measurement,
                                    )
                                    .await
                                {
                                    warn!(%error, "could not store Album loudness");
                                    return true;
                                }
                                if write_tags {
                                    let measurements = track_tags
                                        .iter()
                                        .map(|(track, track_lufs)| {
                                            (*track, *track_lufs, value.integrated_lufs)
                                        })
                                        .collect::<Vec<_>>();
                                    if let Err(error) =
                                        write_local_tags(&state, &measurements).await
                                    {
                                        warn!(%error, "could not write EBU R128 tags");
                                    }
                                }
                            }
                            Err(error) => warn!(%error, "could not combine Album loudness"),
                        }
                    }
                    continue;
                }
                Ok(None) => {}
                Err(error) => {
                    warn!(%error, "could not select Album loudness work");
                    return true;
                }
            }
        }
        match state
            .database
            .next_missing_track_loudness(state.source_key, &ReadCancellation::new())
            .await
        {
            Ok(Some(work)) => match analyze_track(&state, &work, &cancelled).await {
                Ok(value) => {
                    let measurement = LoudnessMeasurement {
                        analysis_key: work.expected_analysis_key,
                        integrated_lufs: value.integrated_lufs,
                        true_peak: value.true_peak,
                        replay_gain_db: None,
                        replay_gain_peak: None,
                    };
                    if let Err(error) = state
                        .database
                        .write_track_analyzed_loudness(
                            state.source_key,
                            work.track_key,
                            &measurement,
                        )
                        .await
                    {
                        warn!(%error, "could not store Track loudness");
                        return true;
                    }
                    if write_tags {
                        let measurements = [(work.track_key, value.integrated_lufs, None)];
                        if let Err(error) = write_local_tags(&state, &measurements).await {
                            warn!(%error, "could not write EBU R128 tags");
                        }
                    }
                }
                Err(error) => {
                    warn!(%error, track_key=%work.track_key, "could not analyze Track loudness");
                    return true;
                }
            },
            Ok(None) => return false,
            Err(error) => {
                warn!(%error, "could not select Track loudness work");
                return true;
            }
        }
    }
}

async fn write_local_tags(
    state: &SelectedSourceState,
    measurements: &[(library::TrackKey, Option<f64>, Option<f64>)],
) -> Result<(), String> {
    if state.configuration.kind != sources::LOCAL_SOURCE_ID {
        return Ok(());
    }
    let source = state
        .source
        .as_ref()
        .ok_or_else(|| "source is unavailable".to_string())?;
    source
        .write_local_r128_tags(&state.database, state.source_key, measurements)
        .await
        .map_err(|error| error.to_string())
}

async fn analyze_track(
    state: &SelectedSourceState,
    work: &TrackLoudnessWork,
    cancelled: &Arc<AtomicBool>,
) -> Result<playback_gstreamer::AnalyzedLoudness, String> {
    analyze_uri(
        state,
        &work.track_object_id,
        work.media_uri.as_deref(),
        work.cue_start_millis,
        work.cue_end_millis,
        cancelled,
    )
    .await
    .map(|analysis| analysis.measurement())
}

async fn analyze_album_track(
    state: &SelectedSourceState,
    work: &AlbumLoudnessTrack,
    cancelled: &Arc<AtomicBool>,
) -> Result<LoudnessAnalysis, String> {
    analyze_uri(
        state,
        &work.track_object_id,
        work.media_uri.as_deref(),
        work.cue_start_millis,
        work.cue_end_millis,
        cancelled,
    )
    .await
}

async fn analyze_uri(
    state: &SelectedSourceState,
    track_object_id: &str,
    media_uri: Option<&str>,
    start: Option<i64>,
    end: Option<i64>,
    cancelled: &Arc<AtomicBool>,
) -> Result<LoudnessAnalysis, String> {
    let request = analysis_stream_request(track_object_id, media_uri, start, end);
    let stream = prepare_stream(state.source.clone(), request).await?;
    let cancelled = Arc::clone(cancelled);
    tokio::task::spawn_blocking(move || {
        analyze_loudness_cancellable(&stream, || cancelled.load(Ordering::Acquire))
    })
    .await
    .map_err(|_| "loudness analysis worker stopped".to_string())?
}

fn analysis_stream_request(
    track_object_id: &str,
    media_uri: Option<&str>,
    start: Option<i64>,
    end: Option<i64>,
) -> StreamRequest {
    let mut request = StreamRequest::new(track_object_id, StreamQuality::Original);
    request.media_uri = media_uri.map(str::to_owned);
    request.cue_start_millis = start.and_then(|value| u64::try_from(value).ok());
    request.cue_end_millis = end.and_then(|value| u64::try_from(value).ok());
    request
}

fn current(
    selected: &WeakActiveSource,
    cancelled: &AtomicBool,
) -> Option<Arc<SelectedSourceState>> {
    if cancelled.load(Ordering::Acquire) {
        return None;
    }
    selected.upgrade()?.resolve()
}

#[cfg(test)]
mod tests {
    use super::{AnalysisFailureBlock, analysis_stream_request};
    use library::SourceKey;
    use playback::SourceSessionEpoch;

    #[test]
    fn provider_loudness_request_keeps_remote_identity_without_fabricating_a_uri() {
        let request = analysis_stream_request("provider-track", None, Some(1_000), Some(2_000));

        assert_eq!(request.track_object_id, "provider-track");
        assert_eq!(request.media_uri, None);
        assert_eq!(request.cue_start_millis, Some(1_000));
        assert_eq!(request.cue_end_millis, Some(2_000));
    }

    #[test]
    fn local_loudness_request_prefers_the_resolved_access_uri() {
        let request =
            analysis_stream_request("provider-track", Some("file:///download.flac"), None, None);

        assert_eq!(request.track_object_id, "provider-track");
        assert_eq!(request.media_uri.as_deref(), Some("file:///download.flac"));
    }

    #[test]
    fn one_failed_analysis_blocks_immediate_restart_until_library_change() {
        let source = SourceKey::from_raw(1);
        let epoch = SourceSessionEpoch::new(7);
        let mut blocked = AnalysisFailureBlock::default();
        assert!(!blocked.blocks(source, epoch));
        blocked.record(source, epoch);
        for _ in 0..10_000 {
            assert!(blocked.blocks(source, epoch));
        }
        blocked.clear();
        assert!(!blocked.blocks(source, epoch));
    }
}
