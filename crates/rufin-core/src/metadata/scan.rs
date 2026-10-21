use super::{EditDestination, EditSession, MetadataCandidate, MetadataEditor};
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

impl MetadataEditor {
    pub(super) async fn scan(self: Arc<Self>) -> Result<Vec<MetadataCandidate>, Error> {
        if !self
            .source
            .shared
            .settings
            .load()
            .ui
            .allows_external_metadata_lookup()
        {
            return Err(Error::Unavailable);
        }
        let cancel = ScanCancellation(Arc::new(AtomicBool::new(false)));
        let cancelled = cancel.0.clone();
        let runtime = self.source.shared.runtime.clone();
        runtime
            .spawn(async move {
                let fingerprint = self
                    .fingerprint_track(self.session.clone().ok_or(Error::Unavailable)?, cancelled)
                    .await?;
                self.scan_matches(fingerprint)
                    .await
                    .map(|matches| matches.candidates)
            })
            .await
            .map_err(scan_error)?
    }

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
        fingerprint: AudioFingerprint,
    ) -> Result<metadata_lookup::FingerprintMatches, Error> {
        let AudioFingerprint {
            encoded: fingerprint,
            duration_seconds: duration,
        } = fingerprint;
        let identity = format!("{duration}:{fingerprint}");
        if let Some((previous, matches)) = self.state().scan.clone()
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
        self.state().scan = Some((identity, matches.clone()));
        Ok(matches)
    }
}

fn scan_error(error: impl std::fmt::Display) -> Error {
    Error::Write(error.to_string())
}
