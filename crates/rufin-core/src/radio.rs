//! Provider-first Radio/AutoDJ with bounded Catalog fallback; Random is Catalog-owned.

use std::sync::Weak;

use crate::playback::RandomPlayRequest;
use library::{Catalog, RadioSeed, ReadCancellation, SourceKey};
use playback::QueueProvenance;
use sources::{Source, SourceRadioSeed};

use crate::playback::random_u64;
use crate::runtime::SelectedLibrary;
use crate::settings::RandomPlaySettings;
use crate::source::SourceOwner;

/// Plays the saved Random Play settings in the selected library. True means nothing matched.
pub async fn play_saved_random(
    owner: &SourceOwner,
    settings: RandomPlaySettings,
    placement: crate::playback::QueuePlacement,
) -> Result<bool, String> {
    owner
        .reply(move |owner, catalog| async move {
            let selected = owner.selected_library().ok_or("No source selected")?;
            let genre = saved_random_genre(
                &catalog,
                &settings,
                &selected.source_id,
                selected.source_key,
                selected.music_folder_object_id.as_deref(),
            )
            .await?;
            let request = RandomPlayRequest {
                placement,
                requested: settings.limit,
                criteria: library::RandomCriteria {
                    min_year: settings.min_year.map(i64::from),
                    max_year: settings.max_year.map(i64::from),
                    genre: genre.map(|genre| library::GenreKey::from_raw(genre.key)),
                    played: settings.played_filter,
                    require_media: false,
                    variation: std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .map_or(0, |duration| duration.as_nanos() as i64),
                },
            };
            let queue = owner.shared.playback()?;
            queue_random(
                &catalog,
                selected.source_key,
                selected.music_folder_key,
                request,
                &queue,
            )
            .await
        })
        .recv()
        .await
        .map_err(|error| error.to_string())?
}

/// Finds the saved Random Play genre when it was saved for this library: a source and one of
/// its music folders.
pub async fn saved_random_genre(
    catalog: &Catalog,
    settings: &RandomPlaySettings,
    source_id: &sources::SourceId,
    source: SourceKey,
    music_folder: Option<&str>,
) -> Result<Option<library::TagRow>, String> {
    let Some(object_id) = settings.selected_genre_id(source_id, music_folder) else {
        return Ok(None);
    };
    let cancellation = ReadCancellation::new();
    let Some(key) = catalog
        .catalog_keys(
            library::CatalogKind::Genre,
            Some(source),
            &[object_id.to_owned()],
            &cancellation,
        )
        .await
        .map_err(|error| error.to_string())?
        .pop()
    else {
        return Ok(None);
    };
    Ok(catalog
        .rows_of(
            &[library::Item::Genre(library::GenreKey::from_raw(key))],
            false,
            &cancellation,
        )
        .await
        .map_err(|error| error.to_string())?
        .into_iter()
        .find_map(|row| match row {
            library::Row::Tag(tag) => Some(tag),
            _ => None,
        }))
}

pub async fn queue_random(
    catalog: &Catalog,
    source: SourceKey,
    folder: Option<library::FolderKey>,
    request: RandomPlayRequest,
    queue: &crate::playback::PlaybackOwner,
) -> Result<bool, String> {
    let cancellation = ReadCancellation::new();
    let order = catalog
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
        library::QueueLoad::track_keys(order, QueueProvenance::Random),
        request.placement,
    ));
    Ok(false)
}

pub(crate) async fn radio_candidates(
    catalog: &Catalog,
    source_owner: Weak<SourceOwner>,
    selected: Option<SelectedLibrary>,
    seed: RadioSeed,
    requested: usize,
    cancellation: &ReadCancellation,
) -> Result<Vec<library::TrackKey>, String> {
    let requested = requested.min(library::QUEUE_CONTEXT_LIMIT);
    let owner = source_owner.upgrade();
    let seed_source = catalog
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
            candidates = catalog
                .recommended_track_keys(source_key, &seed, &ids, cancellation)
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
        let fallback = catalog
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
