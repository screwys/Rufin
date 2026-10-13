//! Provider-first Radio/AutoDJ with bounded Database fallback; Random is Database-owned.

use std::sync::{Arc, Weak};

use crate::playback::{Placement, RadioPlayRequest, RandomPlayRequest};
use library::{Database, RadioSeed, ReadCancellation, SourceKey};
use playback::Provenance;
use sources::{Source, SourceRadioSeed};
use tracing::warn;

use crate::playback::{
    actor::Playback,
    random_u64,
    state::{AutoDjRequest, MaterializationReservation},
};
use crate::runtime::SelectedLibrary;
use crate::source::SourceOwner;

const MANUAL_RADIO_COUNT: usize = 20;

pub fn random_play_request(
    settings: &crate::settings::RandomPlaySettings,
    genre: Option<library::GenreKey>,
    placement: crate::playback::QueuePlacement,
) -> RandomPlayRequest {
    RandomPlayRequest {
        placement,
        requested: settings.limit,
        criteria: library::RandomCriteria {
            min_year: settings.min_year.map(i64::from),
            max_year: settings.max_year.map(i64::from),
            genre,
            played: settings.played_filter,
            require_media: false,
            variation: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |duration| duration.as_nanos() as i64),
        },
    }
}

pub async fn queue_random(
    database: &Database,
    source: SourceKey,
    folder: Option<library::FolderKey>,
    request: RandomPlayRequest,
    queue: &crate::playback::PlaybackOwner,
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
    queue.play_selection(crate::playback::PlayRequest::from_load(
        library::QueueReadRequest::media_uris(order, Provenance::Random),
        request.placement,
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
    let selected = source_owner
        .upgrade()
        .and_then(|owner| owner.selected_library());
    runtime.spawn(async move {
        let candidates = radio_candidates(
            &database,
            source_owner,
            selected,
            RadioSeed::Track(request.seed_media_uri.clone()),
            request.requested_count,
            &ReadCancellation::new(),
        )
        .await;
        match candidates {
            Ok(candidates) => {
                let media = database
                    .queue_items_for_uris(&candidates, &ReadCancellation::new())
                    .await
                    .unwrap_or_default();
                let _ = playback.complete_auto_dj_candidates(
                    request.seed_run,
                    request.seed_media_uri,
                    media,
                    request.requested_count,
                );
            }
            Err(error) => {
                let _ = playback.auto_dj_unavailable(
                    request.seed_run,
                    request.seed_media_uri,
                    Some(error),
                );
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
    let selected = source_owner
        .upgrade()
        .and_then(|owner| owner.selected_library());
    let reservation = playback.reserve_materialization(placement).ok()?;
    Some(runtime.spawn(async move {
        let candidates = radio_candidates(
            &database,
            source_owner,
            selected,
            request.seed,
            MANUAL_RADIO_COUNT,
            &ReadCancellation::new(),
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
    database: Arc<Database>,
    selected: SelectedLibrary,
    playback: Playback,
    request: RandomPlayRequest,
) -> Option<tokio::task::JoinHandle<()>> {
    let placement = request.placement;
    let reservation = playback.reserve_materialization(placement).ok()?;
    Some(runtime.spawn(async move {
        let excluded = reservation
            .current_media_uri
            .iter()
            .cloned()
            .collect::<Vec<_>>();
        let candidates = database
            .random_candidates(
                selected.source_key,
                selected.music_folder_key,
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
    selected: Option<SelectedLibrary>,
    seed: RadioSeed,
    requested: usize,
    cancellation: &ReadCancellation,
) -> Result<Vec<String>, String> {
    let requested = requested.min(library::QUEUE_CONTEXT_LIMIT);
    let owner = source_owner.upgrade();
    let seed_source = database
        .radio_source(&seed, cancellation)
        .await
        .map_err(|error| error.to_string())?;
    let mut candidates = Vec::new();
    let seed_source_key = seed_source.as_ref().map(|(key, _, _)| *key);
    let mut fallback_sources = Vec::new();
    if let Some((source_key, source_id, object_id)) = seed_source {
        let seed_owner = owner.clone();
        let source = tokio::task::spawn_blocking(move || seed_owner?.client(&source_id).ok())
            .await
            .map_err(|error| error.to_string())?;
        let native_seed = source_seed(source.as_deref(), &seed, object_id);
        if let (Some(source), Some(native_seed)) = (source.as_deref(), native_seed)
            && let Ok(ids) = source
                .generated_track_object_ids(&native_seed, requested.min(256))
                .await
        {
            candidates = database
                .admit_radio_candidates(source_key, &seed, &ids, cancellation)
                .await
                .map_err(|error| error.to_string())?;
        }
        fallback_sources.push((source_key, source.is_none()));
    }
    if let Some(selected) = selected
        && Some(selected.source_key) != seed_source_key
    {
        let source_key = selected.source_key;
        let source = tokio::task::spawn_blocking(move || owner?.client(&selected.source_id).ok())
            .await
            .map_err(|error| error.to_string())?;
        fallback_sources.push((source_key, source.is_none()));
    }
    candidates.truncate(requested);
    for (source_key, require_media) in fallback_sources {
        if candidates.len() == requested {
            break;
        }
        let fallback = database
            .radio_candidates(
                source_key,
                seed.clone(),
                &candidates,
                requested - candidates.len(),
                require_media,
                random_u64() as i64,
                cancellation,
            )
            .await
            .map_err(|error| error.to_string())?;
        candidates.extend(fallback);
    }
    Ok(candidates)
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
    reservation: MaterializationReservation,
    placement: Placement,
    candidates: Result<Vec<String>, String>,
    provenance: Provenance,
) {
    match candidates {
        Ok(candidates) if !candidates.is_empty() => {
            let batch = library::QueueReadRequest::media_uris(candidates, provenance);
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
