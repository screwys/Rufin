use super::{EditDestination, EditSession, MetadataCandidate, MetadataEditor, MetadataMatchKind};
use audio_processing::{AudioFingerprint, FINGERPRINT_VERSION, fingerprint_audio};
use library::ReadCancellation;
use playback::{StreamQuality, StreamRequest};
use sources::SourceMetadataError as Error;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

struct ScanCancellation(Arc<AtomicBool>);
impl Drop for ScanCancellation {
    fn drop(&mut self) {
        self.0.store(true, Ordering::Release);
    }
}

#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct MetadataScanResult {
    pub candidates: Vec<MetadataCandidate>,
    pub failure: Option<String>,
}

#[cfg_attr(feature = "bindings", uniffi::export)]
impl MetadataEditor {
    pub async fn scan(self: Arc<Self>) -> Result<MetadataScanResult, Error> {
        if self.draft.kind != sources::SourceEntityKind::Track {
            return Err(Error::Unavailable);
        }
        self.stage_changes().await?;
        let settings = self.source.shared.settings.load().ui;
        if !settings.allows_external_metadata_lookup() {
            return Err(Error::Unavailable);
        }
        let cancel = ScanCancellation(Arc::new(AtomicBool::new(false)));
        let cancelled = cancel.0.clone();
        let runtime = self.source.shared.runtime.clone();
        runtime
            .spawn(async move {
                let mut result = MetadataScanResult {
                    candidates: Vec::new(),
                    failure: None,
                };
                let mut failures = 0_u64;
                let count = self.batch.as_ref().map_or(1, |scope| scope.indices.len());
                for offset in 0..count {
                    if cancelled.load(Ordering::Acquire)
                        || !self
                            .source
                            .shared
                            .settings
                            .load()
                            .ui
                            .allows_external_metadata_lookup()
                    {
                        return Err(Error::Unavailable);
                    }
                    let session = if let Some(scope) = &self.batch {
                        scope.batch.session(scope.indices[offset]).await
                    } else {
                        self.session.clone().ok_or(Error::Unavailable)
                    };
                    let outcome = match session {
                        Ok(session) => self.fingerprint_track(session, cancelled.clone()).await,
                        Err(error) => Err(error),
                    };
                    match outcome {
                        Ok(fingerprint) => {
                            let matches = self.scan_matches(offset, fingerprint).await?;
                            for candidate in matches.candidates {
                                if (count == 1 || candidate.kind == MetadataMatchKind::Release)
                                    && result.candidates.len() < 20
                                    && !result
                                        .candidates
                                        .iter()
                                        .any(|known| known.id == candidate.id)
                                {
                                    result.candidates.push(candidate);
                                }
                            }
                        }
                        Err(error) => {
                            failures += 1;
                            result.failure.get_or_insert_with(|| error.to_string());
                        }
                    }
                }
                if let Some(reason) = result.failure.take() {
                    result.failure = Some(localization::trn_with(
                        "Could not scan {count} track: {reason}",
                        "Could not scan {count} tracks: {reason}",
                        failures,
                        &[("count", &failures.to_string()), ("reason", &reason)],
                    ));
                }
                if self.batch.is_some() {
                    self.reload_batch().await?;
                }
                Ok(result)
            })
            .await
            .map_err(|error| Error::Write(error.to_string()))?
    }
}

impl MetadataEditor {
    async fn fingerprint_track(
        &self,
        session: Arc<EditSession>,
        cancelled: Arc<AtomicBool>,
    ) -> Result<AudioFingerprint, Error> {
        let database = &self.source.shared.database;
        let uri = &session.media_uri;
        database
            .remember_media_uris(std::slice::from_ref(uri))
            .await
            .map_err(scan_error)?;
        let revision = match &session.destination {
            EditDestination::Files(files) => files.audio_revision().await?,
            EditDestination::Server { .. } => {
                database.audio_analysis_key(uri).await.map_err(scan_error)?
            }
        };
        let cached = if let Some(revision) = revision {
            database
                .fingerprint_cache(uri, &revision, FINGERPRINT_VERSION)
                .await
                .map_err(scan_error)?
        } else {
            None
        };
        let (fingerprint, duration) = if let Some(cached) = cached {
            cached
        } else {
            let local = self.source.shared.settings.local_configuration();
            let owner = self.source.clone();
            let stream = crate::playback::prepare_stream(
                database,
                StreamRequest::new(uri, StreamQuality::Original),
                local.as_ref(),
                None,
                move |id| async move { owner.open_client(id).await },
            )
            .await
            .map_err(scan_error)?;
            let duration = database
                .track(uri, &ReadCancellation::new())
                .await
                .map_err(scan_error)?
                .map_or(0, |row| row.metadata.duration_millis.max(0) as u64);
            let fingerprint = self
                .source
                .shared
                .runtime
                .spawn_blocking(move || {
                    fingerprint_audio(&stream, duration, || cancelled.load(Ordering::Acquire))
                })
                .await
                .map_err(scan_error)?
                .map_err(scan_error)?;
            if let Some(revision) = revision {
                database
                    .write_fingerprint_cache(
                        uri,
                        &revision,
                        FINGERPRINT_VERSION,
                        &fingerprint.encoded,
                        fingerprint.duration_seconds,
                    )
                    .await
                    .map_err(scan_error)?;
            }
            (fingerprint.encoded, fingerprint.duration_seconds)
        };
        Ok(AudioFingerprint {
            encoded: fingerprint,
            duration_seconds: duration,
        })
    }

    async fn scan_matches(
        &self,
        offset: usize,
        fingerprint: AudioFingerprint,
    ) -> Result<metadata_lookup::FingerprintMatches, Error> {
        let AudioFingerprint {
            encoded: fingerprint,
            duration_seconds: duration,
        } = fingerprint;
        let identity = format!("{duration}:{fingerprint}");
        let cached = if let Some(scope) = &self.batch {
            scope.batch.read(scope.indices[offset]).await?.state.scan
        } else {
            self.state().scan.clone()
        };
        if let Some((previous, matches)) = cached
            && previous == identity
        {
            return Ok(matches);
        }
        let matches = self
            .source
            .shared
            .runtime
            .spawn_blocking(move || metadata_lookup::lookup_fingerprint(&fingerprint, duration))
            .await
            .map_err(scan_error)?
            .map_err(scan_error)?;
        if let Some(scope) = &self.batch {
            let index = scope.indices[offset];
            let matches = matches.clone();
            scope
                .batch
                .work(move |records| {
                    let mut record = records.read(index)?;
                    record.state.scan = Some((identity, matches));
                    records.write(index, &record)
                })
                .await?;
        } else {
            self.state().scan = Some((identity, matches.clone()));
        }
        Ok(matches)
    }
}

fn scan_error(error: impl std::fmt::Display) -> Error {
    Error::Write(error.to_string())
}
