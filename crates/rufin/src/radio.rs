//! Provider-first Radio/AutoDJ with bounded Database fallback; Random is Database-owned.

use library::{RadioSeed, ReadCancellation};
use playback::{
    AutoDjRequest, Batch, BatchItem, Placement, Playback, Provenance, RadioPlayRequest,
    RandomPlayRequest,
};
use sources::SourceRadioSeed;
use tracing::warn;

use crate::playback::random_u64;
use crate::source::WeakActiveSource;

const MANUAL_RADIO_COUNT: usize = 20;

pub(crate) fn request_auto_dj(
    runtime: tokio::runtime::Handle,
    selected: WeakActiveSource,
    playback: Playback,
    request: AutoDjRequest,
) {
    runtime.spawn(async move {
        let Some(current) = selected.upgrade().and_then(|session| session.resolve()) else {
            return;
        };
        if current.source_key != request.source_id {
            return;
        }
        let candidates = radio_candidates(
            &current,
            RadioSeed::Track(request.seed_track_id),
            request.requested_count,
            false,
        )
        .await;
        match candidates {
            Ok(candidates) => {
                let _ = playback.complete_auto_dj_candidates(
                    request.source_id,
                    request.seed_occurrence,
                    candidates,
                    request.requested_count,
                    random_u64(),
                );
            }
            Err(error) => {
                let _ = playback.auto_dj_unavailable(
                    request.source_id,
                    request.seed_occurrence,
                    Some(error),
                );
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
    let placement: Placement = request.placement.into();
    let reservation = playback.reserve_materialization(placement).ok()?;
    Some(runtime.spawn(async move {
        let Some(current) = selected.upgrade().and_then(|session| session.resolve()) else {
            return;
        };
        let candidates = radio_candidates(&current, request.seed, MANUAL_RADIO_COUNT, true).await;
        complete_materialization(
            playback,
            reservation,
            placement,
            candidates,
            Provenance::Radio,
        );
    }))
}

pub(crate) fn play_random(
    runtime: tokio::runtime::Handle,
    selected: WeakActiveSource,
    playback: Playback,
    request: RandomPlayRequest,
) -> Option<tokio::task::JoinHandle<()>> {
    let placement: Placement = request.placement.into();
    let reservation = playback.reserve_materialization(placement).ok()?;
    Some(runtime.spawn(async move {
        let Some(current) = selected.upgrade().and_then(|session| session.resolve()) else {
            return;
        };
        let excluded = reservation.current_track_id.into_iter().collect::<Vec<_>>();
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
        );
    }))
}

async fn radio_candidates(
    selected: &crate::source::SelectedSourceState,
    seed: RadioSeed,
    requested: usize,
    include_seed: bool,
) -> Result<Vec<library::TrackKey>, String> {
    let native_seed = source_seed(selected, seed).await?;
    let mut native = if let (Some(source), Some(seed)) = (&selected.source, native_seed) {
        match source
            .generated_track_object_ids(&seed, requested.min(256))
            .await
        {
            Ok(ids) => selected
                .database
                .track_keys_by_objects(selected.source_key, &ids, &ReadCancellation::new())
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
    if !include_seed && let RadioSeed::Track(track) = seed {
        excluded.push(track);
    }
    excluded.sort_unstable();
    excluded.dedup();
    let fallback = selected
        .database
        .radio_candidates(
            selected.source_key,
            seed,
            &excluded,
            requested - native.len(),
            selected.source.is_none(),
            random_u64() as i64,
            &ReadCancellation::new(),
        )
        .await
        .map_err(|error| error.to_string())?;
    native.extend(fallback);
    Ok(native)
}

async fn source_seed(
    selected: &crate::source::SelectedSourceState,
    seed: RadioSeed,
) -> Result<Option<SourceRadioSeed>, String> {
    let cancel = ReadCancellation::new();
    Ok(match seed {
        RadioSeed::Track(key) => selected
            .database
            .track_rows(selected.source_key, &[key], &cancel)
            .await
            .map_err(|e| e.to_string())?
            .pop()
            .map(|row| SourceRadioSeed::Track(row.object_id)),
        RadioSeed::Album(key) => selected
            .database
            .album_rows(selected.source_key, &[key], None, &cancel)
            .await
            .map_err(|e| e.to_string())?
            .pop()
            .map(|row| SourceRadioSeed::Album(row.object_id)),
        RadioSeed::Artist(key) => selected
            .database
            .artist_rows(selected.source_key, &[key], false, None, &cancel)
            .await
            .map_err(|e| e.to_string())?
            .pop()
            .map(|row| SourceRadioSeed::Artist(row.object_id)),
        RadioSeed::AlbumArtist(key) => selected
            .database
            .artist_rows(selected.source_key, &[key], true, None, &cancel)
            .await
            .map_err(|e| e.to_string())?
            .pop()
            .map(|row| SourceRadioSeed::Artist(row.object_id)),
        RadioSeed::Genre(key) => selected
            .database
            .genre_rows(selected.source_key, &[key], None, &cancel)
            .await
            .map_err(|e| e.to_string())?
            .pop()
            .map(|row| SourceRadioSeed::Genre(row.object_id)),
        RadioSeed::Playlist(key) => selected
            .database
            .playlist_rows(selected.source_key, &[key], None, &cancel)
            .await
            .map_err(|e| e.to_string())?
            .pop()
            .map(|row| SourceRadioSeed::Playlist(row.object_id)),
    })
}

fn complete_materialization(
    playback: Playback,
    reservation: playback::MaterializationReservation,
    placement: Placement,
    candidates: Result<Vec<library::TrackKey>, String>,
    provenance: Provenance,
) {
    match candidates {
        Ok(candidates) if !candidates.is_empty() => {
            let batch = Batch::new(
                candidates
                    .into_iter()
                    .map(|key| BatchItem::new(key, provenance.clone()))
                    .collect(),
            );
            if let Err(error) = playback.complete_materialization(
                reservation.id,
                reservation.source_id,
                batch,
                placement,
                None,
            ) {
                warn!(%error, "could not complete queue materialization");
            }
        }
        Ok(_) => {
            let _ =
                playback.cancel_materialization(reservation.id, reservation.source_id, placement);
        }
        Err(error) => {
            let _ = playback.fail_materialization(
                reservation.id,
                reservation.source_id,
                placement,
                error,
            );
        }
    }
}
