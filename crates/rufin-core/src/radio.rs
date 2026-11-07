//! Provider-first Radio/AutoDJ with bounded Catalog fallback; Random is Catalog-owned.

use std::sync::Arc;

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
    queue: &Arc<crate::playback::PlaybackOwner>,
    settings: RandomPlaySettings,
    placement: crate::playback::QueuePlacement,
) -> Result<bool, String> {
    let queue = Arc::clone(queue);
    queue
        .source
        .clone()
        .reply(move |owner, catalog| async move {
            let selected = owner.selected_library().ok_or("No source selected")?;
            let genre = saved_random_genre(&catalog, &settings, &selected).await?;
            let request = RandomPlayRequest {
                placement,
                requested: settings.limit,
                criteria: library::RandomCriteria {
                    min_year: settings.min_year.map(i64::from),
                    max_year: settings.max_year.map(i64::from),
                    genre: genre.map(|genre| library::GenreKey::from_raw(genre.key)),
                    played: settings.played_filter,
                    variation: std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .map_or(0, |duration| duration.as_nanos() as i64),
                },
            };
            queue_random(&catalog, &selected, request, &queue).await
        })
        .recv()
        .await
        .map_err(|error| error.to_string())?
}

/// The source and music folder Random Play reads in `selected`: it is query-built, so the
/// folder narrows it (decisions.md, 2026-11-06).
fn random_library(selected: &SelectedLibrary) -> (SourceKey, Option<library::FolderKey>) {
    (selected.source_key, selected.music_folder_key)
}

/// The genres Random Play offers in `selected` whose names hold `filter`, and how many there
/// are.
pub async fn random_genres(
    catalog: &Catalog,
    selected: &SelectedLibrary,
    filter: &str,
    range: std::ops::Range<usize>,
    cancellation: &ReadCancellation,
) -> library::LibraryResult<(usize, Vec<library::GenreChoice>)> {
    let (source, folder) = random_library(selected);
    catalog
        .genre_choices(source, folder, filter, range, cancellation)
        .await
}

/// Finds the saved Random Play genre when it was saved for `selected`: a source and one of
/// its music folders.
pub async fn saved_random_genre(
    catalog: &Catalog,
    settings: &RandomPlaySettings,
    selected: &SelectedLibrary,
) -> Result<Option<library::TagRow>, String> {
    let Some(object_id) = settings.selected_genre_id(
        &selected.source_id,
        selected.music_folder_object_id.as_deref(),
    ) else {
        return Ok(None);
    };
    let cancellation = ReadCancellation::new();
    let Some(key) = catalog
        .catalog_keys(
            library::CatalogKind::Genre,
            Some(selected.source_key),
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

/// Plays what `request` picks at random in `selected`. True means nothing matched.
pub async fn queue_random(
    catalog: &Catalog,
    selected: &SelectedLibrary,
    request: RandomPlayRequest,
    queue: &crate::playback::PlaybackOwner,
) -> Result<bool, String> {
    let cancellation = ReadCancellation::new();
    let (source, folder) = random_library(selected);
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
    source_owner: Arc<SourceOwner>,
    selected: Option<SelectedLibrary>,
    seed: RadioSeed,
    requested: usize,
    cancellation: &ReadCancellation,
) -> Result<Vec<library::TrackKey>, String> {
    let requested = requested.min(library::QUEUE_CONTEXT_LIMIT);
    let seed_source = catalog
        .radio_source(&seed, cancellation)
        .await
        .map_err(|error| error.to_string())?;
    let mut candidates = Vec::new();
    let seed_source_key = seed_source.as_ref().map(|(key, _, _)| *key);
    let mut fallback_sources = Vec::new();
    if let Some((source_key, source_id, object_id)) = seed_source {
        let seed_owner = Arc::clone(&source_owner);
        let source = tokio::task::spawn_blocking(move || seed_owner.client(&source_id).ok())
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
        fallback_sources.push(source_key);
    }
    if let Some(selected) = selected
        && Some(selected.source_key) != seed_source_key
    {
        fallback_sources.push(selected.source_key);
    }
    candidates.truncate(requested);
    for source_key in fallback_sources {
        if candidates.len() == requested {
            break;
        }
        let fallback = catalog
            .radio_candidates(
                source_key,
                seed.clone(),
                &candidates,
                requested - candidates.len(),
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
