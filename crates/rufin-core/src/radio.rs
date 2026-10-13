//! Provider-first Radio/AutoDJ with bounded Database fallback; Random is Database-owned.

use std::sync::{Arc, Weak};

use crate::playback::RandomPlayRequest;
use library::{Database, RadioSeed, ReadCancellation, SourceKey};
use playback::Provenance;
use sources::{Source, SourceRadioSeed};

use crate::playback::{actor::Playback, random_u64, state::AutoDjRequest};
use crate::runtime::SelectedLibrary;
use crate::source::SourceOwner;

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
    queue.play_selection(crate::playback::PlayRequest::from_input(
        library::QueueLoad::media_uris(order, Provenance::Random),
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
