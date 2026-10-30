//! Sequential bounded loudness analysis for one missing Track or Album. While normalization
//! is EBU R128 it follows the change feed: tracks added to the selected source start a pass
//! over what is missing.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, Weak};

use audio_processing::{
    AnalyzedLoudness, LoudnessAnalysis, album_loudness, analyze_loudness_cancellable,
};
use library::{Catalog, Change, LoudnessMeasurement, Playable, Scope, SourceKey, TrackKey};
use playback::{LoudnessNormalizationScope, StreamQuality, StreamRequest};
use sources::SourceConfiguration;
use tracing::{debug, info, warn};

use crate::playback::{PlaybackOwner, prepare_stream};
use crate::settings::SettingsFile;
use crate::source::SourceOwner;

struct ActiveAnalysis {
    source_key: SourceKey,
    source_owner: Weak<SourceOwner>,
    cancelled: Arc<AtomicBool>,
    task: Option<tokio::task::AbortHandle>,
    restart: bool,
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
    settings: SettingsFile,
    active: Mutex<Option<ActiveAnalysis>>,
    blocked: Mutex<AnalysisFailureBlock>,
}

#[derive(Default)]
struct AnalysisFailureBlock(Option<SourceKey>);

impl AnalysisFailureBlock {
    fn blocks(&self, source: SourceKey) -> bool {
        self.0 == Some(source)
    }

    fn record(&mut self, source: SourceKey) {
        self.0 = Some(source);
    }

    fn clear(&mut self) {
        self.0 = None;
    }
}

/// While normalization is EBU R128, tracks added to the selected source start a pass over what
/// is missing.
pub(crate) async fn follow(playback: Arc<PlaybackOwner>, changes: Option<Vec<Arc<Change>>>) {
    if !playback.ebu_r128() {
        return;
    }
    let Some(changes) = changes else {
        playback.loudness.start(playback.source_owner());
        return;
    };
    let source = playback.source_owner();
    let selected = source
        .as_ref()
        .and_then(|source| source.selected_library())
        .map(|selected| selected.source_key);
    let added = changes
        .iter()
        .flat_map(|change| &change.scopes)
        .any(|scope| match scope {
            Scope::Reload => true,
            Scope::Tracks {
                source: Some(source),
                added,
                ..
            } => !added.is_empty() && Some(*source) == selected,
            _ => false,
        });
    if added {
        playback.loudness.library_changed(source);
    }
}

impl LoudnessAnalysisOwner {
    pub(crate) fn new(runtime: tokio::runtime::Handle, settings: SettingsFile) -> Arc<Self> {
        Arc::new(Self {
            runtime,
            settings,
            active: Mutex::new(None),
            blocked: Mutex::new(AnalysisFailureBlock::default()),
        })
    }

    /// Analyzes what the selected source misses, unless that runs already.
    pub(crate) fn start(self: &Arc<Self>, source_owner: Option<Arc<SourceOwner>>) {
        let Some(source_owner) = source_owner else {
            self.cancel();
            return;
        };
        let Some(selected) = source_owner.selected_library() else {
            self.cancel();
            return;
        };
        let source_key = selected.source_key;
        let Some(configuration) = source_owner.configuration(&selected.source_id) else {
            self.cancel();
            return;
        };
        if self
            .blocked
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .blocks(source_key)
        {
            return;
        }
        let playback = self.settings.load().ui.playback;
        let (scope, write_tags) = (
            playback.loudness_normalization_scope,
            playback.write_ebu_r128_tags,
        );
        let cancelled = Arc::new(AtomicBool::new(false));
        {
            let mut active = self.active.lock().unwrap_or_else(|p| p.into_inner());
            if active
                .as_ref()
                .is_some_and(|current| current.source_key == source_key)
            {
                return;
            }
            if let Some(previous) = active.take() {
                previous.cancel();
            }
            *active = Some(ActiveAnalysis {
                source_key,
                source_owner: Arc::downgrade(&source_owner),
                cancelled: Arc::clone(&cancelled),
                task: None,
                restart: false,
            });
        }
        let owner = Arc::downgrade(self);
        let task_cancelled = Arc::clone(&cancelled);
        let task = self.runtime.spawn(async move {
            info!(%source_key, "analyzing missing loudness data");
            let failed = analyze_selected(
                source_owner,
                source_key,
                configuration,
                scope,
                write_tags,
                Arc::clone(&task_cancelled),
            )
            .await;
            if let Some(owner) = owner.upgrade() {
                owner.finish(&task_cancelled, failed);
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

    /// The selected source or its tracks changed: what it misses is analyzed again.
    pub(crate) fn library_changed(self: &Arc<Self>, source_owner: Option<Arc<SourceOwner>>) {
        let Some(selected) = source_owner
            .as_ref()
            .and_then(|source_owner| source_owner.selected_library())
        else {
            self.cancel();
            return;
        };
        self.blocked
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clear();
        let mut active = self.active.lock().unwrap_or_else(|p| p.into_inner());
        if let Some(current) = active
            .as_mut()
            .filter(|current| current.source_key == selected.source_key)
        {
            current.restart = true;
            return;
        }
        drop(active);
        self.start(source_owner);
    }

    pub(crate) fn cancel(&self) {
        if let Some(active) = self.active.lock().unwrap_or_else(|p| p.into_inner()).take() {
            active.cancel();
        }
    }

    fn finish(self: &Arc<Self>, cancelled: &Arc<AtomicBool>, failed: bool) {
        let mut active = self.active.lock().unwrap_or_else(|p| p.into_inner());
        let completed = active
            .as_ref()
            .is_some_and(|current| Arc::ptr_eq(&current.cancelled, cancelled))
            .then(|| active.take())
            .flatten();
        let restart = completed.as_ref().and_then(|current| {
            current
                .restart
                .then(|| current.source_owner.upgrade())
                .flatten()
        });
        if failed && let Some(current) = completed.as_ref() {
            self.blocked
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .record(current.source_key);
        }
        drop(active);
        if !failed && let Some(source_owner) = restart {
            self.start(Some(source_owner));
        }
    }
}

async fn analyze_selected(
    source_owner: Arc<SourceOwner>,
    source_key: SourceKey,
    configuration: SourceConfiguration,
    scope: LoudnessNormalizationScope,
    write_tags: bool,
    cancelled: Arc<AtomicBool>,
) -> bool {
    let catalog = Arc::clone(&source_owner.shared.catalog);
    if write_tags && configuration.is_file_library() {
        let owner = Arc::clone(&source_owner);
        let source_id = configuration.source_id.clone();
        let source = tokio::task::spawn_blocking(move || owner.client(&source_id)).await;
        if let Ok(Ok(source)) = source {
            let result = async {
                source
                    .backfill_file_r128_tags(&catalog, source_key)
                    .await
                    .map_err(|error| error.to_string())
            }
            .await;
            if let Err(error) = result {
                warn!(%error, "could not write stored EBU R128 tags");
            }
        }
    }
    let analysis = Analysis {
        source_owner: &source_owner,
        catalog: &catalog,
        configuration: &configuration,
        source_key,
        write_tags,
        cancelled: &cancelled,
    };
    let result = async {
        if scope == LoudnessNormalizationScope::Album {
            analysis.albums().await?;
        }
        analysis.tracks().await
    };
    match result.await {
        Ok(()) => false,
        Err(error) => {
            warn!(%error, "loudness analysis stopped");
            true
        }
    }
}

struct Analysis<'a> {
    source_owner: &'a Arc<SourceOwner>,
    catalog: &'a Catalog,
    configuration: &'a SourceConfiguration,
    source_key: SourceKey,
    write_tags: bool,
    cancelled: &'a Arc<AtomicBool>,
}

impl Analysis<'_> {
    /// Albums whose members' loudness is not stored: every member is decoded, unless hashing
    /// the members finds the album stored.
    async fn albums(&self) -> Result<(), String> {
        let mut after = None;
        loop {
            let page = self
                .catalog
                .album_loudness_work(self.source_key, after, 16)
                .await
                .map_err(|error| format!("could not select Album loudness work: {error}"))?;
            let Some(last) = page.last() else {
                return Ok(());
            };
            after = Some(last.0);
            for (album, tracks) in page {
                if self.cancelled.load(Ordering::Acquire) {
                    return Ok(());
                }
                let mut playables = Vec::with_capacity(tracks.len());
                for track in tracks {
                    playables.extend(hashed_playable(self.catalog, track).await?);
                }
                // The last read saw every hash recorded before it.
                if playables
                    .last()
                    .is_none_or(|playable| playable.gains.album_analysed.is_some())
                {
                    continue;
                }
                let Some(hashes) = playables
                    .iter()
                    .map(|playable| playable.audio_hash)
                    .collect::<Option<Vec<_>>>()
                else {
                    continue;
                };
                let mut analyses = Vec::with_capacity(playables.len());
                let mut track_tags = Vec::with_capacity(playables.len());
                for (playable, hash) in playables.iter().zip(hashes) {
                    let analysis = self.analyze(playable).await?;
                    let value = analysis.measurement();
                    self.store_track(hash, value).await?;
                    track_tags.push((playable.track, value.integrated_lufs));
                    analyses.push(analysis);
                }
                let value = match album_loudness(&analyses) {
                    Ok(value) => value,
                    Err(error) => {
                        warn!(%error, "could not combine Album loudness");
                        continue;
                    }
                };
                self.catalog
                    .put_album_loudness(album, &measurement(value))
                    .await
                    .map_err(|error| format!("could not store Album loudness: {error}"))?;
                let tags = track_tags
                    .into_iter()
                    .map(|(track, lufs)| (track, lufs, value.integrated_lufs))
                    .collect::<Vec<_>>();
                self.write_tags(&tags).await;
            }
        }
    }

    /// Tracks whose loudness is not stored for their audio.
    async fn tracks(&self) -> Result<(), String> {
        let mut after = None;
        loop {
            let page = self
                .catalog
                .loudness_work(self.source_key, after, 64)
                .await
                .map_err(|error| format!("could not select Track loudness work: {error}"))?;
            let Some(&last) = page.last() else {
                return Ok(());
            };
            after = Some(last);
            for track in page {
                if self.cancelled.load(Ordering::Acquire) {
                    return Ok(());
                }
                let Some(playable) = hashed_playable(self.catalog, track)
                    .await?
                    .filter(|playable| playable.gains.analysed.is_none())
                else {
                    continue;
                };
                let Some(hash) = playable.audio_hash else {
                    continue;
                };
                let value = self.analyze(&playable).await?.measurement();
                self.store_track(hash, value).await?;
                self.write_tags(&[(track, value.integrated_lufs, None)])
                    .await;
            }
        }
    }

    async fn store_track(&self, hash: [u8; 32], value: AnalyzedLoudness) -> Result<(), String> {
        self.catalog
            .put_loudness(hash, &measurement(value))
            .await
            .map(drop)
            .map_err(|error| format!("could not store Track loudness: {error}"))
    }

    async fn write_tags(&self, measurements: &[(TrackKey, Option<f64>, Option<f64>)]) {
        if !self.write_tags || !self.configuration.is_file_library() {
            return;
        }
        let owner = Arc::clone(self.source_owner);
        let source_id = self.configuration.source_id.clone();
        let result = async {
            let source = tokio::task::spawn_blocking(move || owner.client(&source_id))
                .await
                .map_err(|error| error.to_string())??;
            source
                .write_file_r128_tags(self.catalog, self.source_key, measurements)
                .await
                .map_err(|error| error.to_string())
        }
        .await;
        if let Err(error) = result {
            warn!(%error, "could not write EBU R128 tags");
        }
    }

    async fn analyze(&self, playable: &Playable) -> Result<LoudnessAnalysis, String> {
        debug!(track = %playable.track, "decoding for loudness");
        let owner = Arc::clone(self.source_owner);
        let stream = prepare_stream(
            self.catalog,
            playable,
            StreamRequest::new(&playable.media_uri, StreamQuality::Original),
            Some(self.configuration),
            None,
            move |source_id| async move {
                tokio::task::spawn_blocking(move || owner.client(&source_id))
                    .await
                    .map_err(|error| error.to_string())?
            },
        )
        .await;
        let cancelled = Arc::clone(self.cancelled);
        let analysis = async {
            tokio::task::spawn_blocking(move || {
                analyze_loudness_cancellable(&stream?, || cancelled.load(Ordering::Acquire))
            })
            .await
            .map_err(|_| "the worker stopped".to_string())?
        };
        analysis.await.map_err(|error| {
            format!(
                "could not analyze Track {} loudness: {error}",
                playable.track
            )
        })
    }
}

fn measurement(value: AnalyzedLoudness) -> LoudnessMeasurement {
    LoudnessMeasurement {
        analysis_key: [0; 32],
        integrated_lufs: value.integrated_lufs,
        true_peak: value.true_peak,
        replay_gain_db: None,
        replay_gain_peak: None,
    }
}

/// A track's playable with its audio hash. A Local file whose hash is not known is read once:
/// its payload hash, with the CUE window, is recorded against the audio revision, and the
/// playable is read again so it carries what is stored for that audio.
pub(crate) async fn hashed_playable(
    catalog: &Catalog,
    track: TrackKey,
) -> Result<Option<Playable>, String> {
    let playable = catalog
        .playable(track, "original")
        .await
        .map_err(|error| error.to_string())?;
    let Some((revision, path, window)) = playable.as_ref().and_then(|playable| {
        let path = url::Url::parse(&playable.local_uri()?)
            .ok()?
            .to_file_path()
            .ok()?;
        playable
            .audio_hash
            .is_none()
            .then_some((playable.audio_revision?, path, playable.window))
    }) else {
        return Ok(playable);
    };
    let payload = tokio::task::spawn_blocking(move || audio_processing::payload_hash(&path))
        .await
        .map_err(|error| error.to_string())?
        .map_err(|error| format!("could not read the audio of Track {track}: {error}"))?;
    let hash = match window {
        Some((start, end)) => *blake3::Hasher::new()
            .update(&payload)
            .update(&start.to_le_bytes())
            .update(&end.to_le_bytes())
            .finalize()
            .as_bytes(),
        None => payload,
    };
    info!(%track, "read the audio payload for its hash");
    catalog
        .put_audio_hash(track, revision, hash)
        .await
        .map_err(|error| error.to_string())?;
    catalog
        .playable(track, "original")
        .await
        .map_err(|error| error.to_string())
}

#[cfg(test)]
mod tests {
    use super::AnalysisFailureBlock;
    use library::SourceKey;

    #[test]
    fn one_failed_analysis_blocks_immediate_restart_until_library_change() {
        let source = SourceKey::from_raw(1);
        let mut blocked = AnalysisFailureBlock::default();
        assert!(!blocked.blocks(source));
        blocked.record(source);
        for _ in 0..10_000 {
            assert!(blocked.blocks(source));
        }
        blocked.clear();
        assert!(!blocked.blocks(source));
    }
}
