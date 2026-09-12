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

pub async fn queue_random(
    database: &Database,
    source: SourceKey,
    folder: Option<library::FolderKey>,
    request: RandomPlayRequest,
    queue: &playback::QueueHandle,
) -> Result<bool, String> {
    let cancellation = ReadCancellation::new();
    let order = database
        .random_candidates(
            source,
            folder,
            &request.criteria,
            &[],
            request.requested,
            &cancellation,
        )
        .await
        .map_err(|error| error.to_string())?;
    if order.is_empty() {
        return Ok(true);
    }
    queue.play(playback::PlayRequest::ordered(
        library::QueueInput::MediaUris {
            order: order.into(),
            provenance: Provenance::Random,
        },
        0,
        request.placement,
        false,
    ));
    Ok(false)
}

pub(crate) fn request_auto_dj(
    runtime: tokio::runtime::Handle,
    database: Arc<Database>,
    source_owner: Weak<SourceOwner>,
    playback: Playback,
    request: AutoDjRequest,
) {
    runtime.spawn(async move {
        let candidates = radio_candidates(
            &database,
            source_owner,
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
    database: Arc<Database>,
    source_owner: Weak<SourceOwner>,
    playback: Playback,
    request: RadioPlayRequest,
) -> Option<tokio::task::JoinHandle<()>> {
    let placement = request.placement;
    let reservation = playback.reserve_materialization(placement).ok()?;
    Some(runtime.spawn(async move {
        let candidates =
            radio_candidates(&database, source_owner, request.seed, MANUAL_RADIO_COUNT).await;
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

pub(crate) async fn radio_candidates(
    database: &Database,
    source_owner: Weak<SourceOwner>,
    seed: RadioSeed,
    requested: usize,
) -> Result<Vec<String>, String> {
    let requested = requested.min(library::QUEUE_CONTEXT_LIMIT);
    let (source_key, source_id, object_id) = database
        .radio_source(&seed, &ReadCancellation::new())
        .await
        .map_err(|error| error.to_string())?
        .ok_or("Radio seed is unavailable")?;
    let source =
        tokio::task::spawn_blocking(move || source_owner.upgrade()?.client(&source_id).ok())
            .await
            .map_err(|error| error.to_string())?;
    let source = source.as_deref();
    let native_seed = source_seed(source, &seed, object_id);
    let mut native = if let (Some(source), Some(native_seed)) = (source, native_seed) {
        match source
            .generated_track_object_ids(&native_seed, requested.min(256))
            .await
        {
            Ok(ids) => database
                .admit_radio_candidates(source_key, &seed, &ids, &ReadCancellation::new())
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

fn source_seed(
    source: Option<&Source>,
    seed: &RadioSeed,
    object_id: Option<String>,
) -> Option<SourceRadioSeed> {
    match seed {
        RadioSeed::Track(media_uri) => {
            library::source_entity_parts(media_uri).and_then(|(source_id, kind, object_id)| {
                (kind == "track" && source.is_some_and(|source| source.source_id() == &source_id))
                    .then_some(SourceRadioSeed::Track(object_id))
            })
        }
        RadioSeed::Album(_) => object_id.map(SourceRadioSeed::Album),
        RadioSeed::Artist(_) | RadioSeed::AlbumArtist(_) => object_id.map(SourceRadioSeed::Artist),
        RadioSeed::Genre(_) => object_id.map(SourceRadioSeed::Genre),
        RadioSeed::Playlist(_) => object_id.map(SourceRadioSeed::Playlist),
    }
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
