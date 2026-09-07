//! Provider-first Radio/AutoDJ with bounded Database fallback; Random is Database-owned.

use std::sync::{Arc, Weak};

use library::{Database, RadioSeed, ReadCancellation, SourceKey};
use playback::{
    AutoDjRequest, Batch, Placement, Playback, Provenance, RadioPlayRequest, RandomPlayRequest,
};
use sources::{Source, SourceRadioSeed};
use tracing::warn;

use crate::playback::random_u64;
use crate::source::{SourceOwner, WeakActiveSource};

const MANUAL_RADIO_COUNT: usize = 20;

pub(crate) fn request_auto_dj(
    runtime: tokio::runtime::Handle,
    database: Arc<Database>,
    source_owner: Weak<SourceOwner>,
    playback: Playback,
    request: AutoDjRequest,
) {
    runtime.spawn(async move {
        let seed_source = match library::source_entity_parts(&request.seed_media_uri) {
            Some((source_id, kind, _)) if kind == "track" => database
                .source_identity_key(&source_id)
                .await
                .ok()
                .flatten()
                .map(|key| (key, Some(source_id))),
            Some(_) => None,
            None => database
                .track_row_by_uri(&request.seed_media_uri, &ReadCancellation::new())
                .await
                .ok()
                .flatten()
                .map(|track| (track.source_key, None)),
        };
        let Some((source_key, source_id)) = seed_source else {
            let _ = playback.auto_dj_unavailable(
                request.seed_occurrence,
                Some("Auto DJ seed is unavailable".to_string()),
            );
            return;
        };
        let source = if let Some(source_id) = source_id {
            tokio::task::spawn_blocking(move || source_owner.upgrade()?.client(&source_id).ok())
                .await
                .ok()
                .flatten()
        } else {
            None
        };
        let candidates = radio_candidates(
            &database,
            source_key,
            source.as_deref(),
            RadioSeed::Track(request.seed_media_uri),
            request.requested_count,
        )
        .await;
        match candidates {
            Ok(candidates) => {
                let media = database
                    .queue_items_for_uris(&candidates, &ReadCancellation::new())
                    .await
                    .unwrap_or_default();
                let _ = playback.complete_auto_dj_candidates(
                    request.seed_occurrence,
                    media,
                    request.requested_count,
                    random_u64(),
                );
            }
            Err(error) => {
                let _ = playback.auto_dj_unavailable(request.seed_occurrence, Some(error));
            }
        }
    });
}

pub(crate) fn play_radio(
    runtime: tokio::runtime::Handle,
    selected: WeakActiveSource,
    playback: Playback,
    request: RadioPlayRequest,
) -> Option<tokio::task::JoinHandle<()>> {
    let placement = request.placement;
    let current = selected.upgrade()?.resolve()?;
    let reservation = playback.reserve_materialization(placement).ok()?;
    Some(runtime.spawn(async move {
        let candidates = radio_candidates(
            &current.database,
            current.source_key,
            current.source.as_deref(),
            request.seed,
            MANUAL_RADIO_COUNT,
        )
        .await;
        complete_materialization(
            playback,
            reservation,
            placement,
            candidates,
            Provenance::Radio,
        )
        .await;
    }))
}

pub(crate) fn play_random(
    runtime: tokio::runtime::Handle,
    selected: WeakActiveSource,
    playback: Playback,
    request: RandomPlayRequest,
) -> Option<tokio::task::JoinHandle<()>> {
    let placement = request.placement;
    let current = selected.upgrade()?.resolve()?;
    let reservation = playback.reserve_materialization(placement).ok()?;
    Some(runtime.spawn(async move {
        let excluded = reservation
            .current_media_uri
            .iter()
            .cloned()
            .collect::<Vec<_>>();
        let candidates = current
            .database
            .random_candidates(
                current.source_key,
                current.music_folder_key,
                &request.criteria,
                &excluded,
                request.requested,
                &ReadCancellation::new(),
            )
            .await
            .map_err(|error| error.to_string());
        complete_materialization(
            playback,
            reservation,
            placement,
            candidates,
            Provenance::Random,
        )
        .await;
    }))
}

async fn radio_candidates(
    database: &Database,
    source_key: SourceKey,
    source: Option<&Source>,
    seed: RadioSeed,
    requested: usize,
) -> Result<Vec<String>, String> {
    let requested = requested.min(library::QUEUE_CONTEXT_LIMIT);
    let native_seed = source_seed(database, source_key, source, &seed).await?;
    let mut native = if let (Some(source), Some(seed)) = (source, native_seed) {
        match source
            .generated_track_object_ids(&seed, requested.min(256))
            .await
        {
            Ok(ids) => database
                .track_media_uris_by_objects(source_key, &ids, &ReadCancellation::new())
                .await
                .map_err(|error| error.to_string())?,
            Err(_) => Vec::new(),
        }
    } else {
        Vec::new()
    };
    native.truncate(requested);
    if native.len() == requested {
        return Ok(native);
    }
    let mut excluded = native.clone();
    excluded.sort();
    excluded.dedup();
    let fallback = database
        .radio_candidates(
            source_key,
            seed,
            &excluded,
            requested - native.len(),
            source.is_none(),
            random_u64() as i64,
            &ReadCancellation::new(),
        )
        .await
        .map_err(|error| error.to_string())?;
    native.extend(fallback);
    Ok(native)
}

async fn source_seed(
    database: &Database,
    source_key: SourceKey,
    source: Option<&Source>,
    seed: &RadioSeed,
) -> Result<Option<SourceRadioSeed>, String> {
    let cancel = ReadCancellation::new();
    Ok(match seed {
        RadioSeed::Track(media_uri) => {
            library::source_entity_parts(media_uri).and_then(|(source_id, kind, object_id)| {
                (kind == "track" && source.is_some_and(|source| source.source_id() == &source_id))
                    .then_some(SourceRadioSeed::Track(object_id))
            })
        }
        RadioSeed::Album(key) => database
            .album_rows(source_key, &[*key], None, &cancel)
            .await
            .map_err(|e| e.to_string())?
            .pop()
            .map(|row| SourceRadioSeed::Album(row.object_id)),
        RadioSeed::Artist(key) => database
            .artist_rows(source_key, &[*key], false, None, &cancel)
            .await
            .map_err(|e| e.to_string())?
            .pop()
            .map(|row| SourceRadioSeed::Artist(row.object_id)),
        RadioSeed::AlbumArtist(key) => database
            .artist_rows(source_key, &[*key], true, None, &cancel)
            .await
            .map_err(|e| e.to_string())?
            .pop()
            .map(|row| SourceRadioSeed::Artist(row.object_id)),
        RadioSeed::Genre(key) => database
            .genre_rows(source_key, &[*key], None, &cancel)
            .await
            .map_err(|e| e.to_string())?
            .pop()
            .map(|row| SourceRadioSeed::Genre(row.object_id)),
        RadioSeed::Playlist(key) => database
            .playlist_rows(&[*key], &cancel)
            .await
            .map_err(|e| e.to_string())?
            .pop()
            .map(|row| SourceRadioSeed::Playlist(row.object_id)),
    })
}

async fn complete_materialization(
    playback: Playback,
    reservation: playback::MaterializationReservation,
    placement: Placement,
    candidates: Result<Vec<String>, String>,
    provenance: Provenance,
) {
    match candidates {
        Ok(candidates) if !candidates.is_empty() => {
            let batch = Batch::from_input(library::QueueInput::MediaUris {
                order: candidates.into(),
                provenance,
            });
            if let Err(error) = playback.complete_materialization(reservation.id, batch, placement)
            {
                warn!(%error, "could not complete queue materialization");
            }
        }
        Ok(_) => {
            let _ = playback.cancel_materialization(reservation.id, placement);
        }
        Err(error) => {
            let _ = playback.fail_materialization(reservation.id, placement, error);
        }
    }
}
