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
                requested: if settings.albums {
                    settings.album_limit
                } else {
                    settings.limit
                },
                criteria: library::RandomCriteria {
                    min_year: settings.min_year.map(i64::from),
                    max_year: settings.max_year.map(i64::from),
                    genre: genre.map(|genre| library::GenreKey::from_raw(genre.key)),
                    played: settings.played_filter,
                    albums: settings.albums,
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
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_secs() as i64);
    let pick = catalog
        .random_pick(
            source,
            folder,
            &request.criteria,
            request.requested,
            now,
            random_u64() as i64,
            &cancellation,
        )
        .await
        .map_err(|error| error.to_string())?;
    let load = match pick {
        library::RandomPick::Tracks(tracks) if !tracks.is_empty() => {
            library::QueueLoad::track_keys(tracks, QueueProvenance::Random)
        }
        // Each album whole, in its own order, as its page plays it.
        library::RandomPick::Albums(albums) if !albums.is_empty() => library::QueueLoad::Tracks(
            albums
                .into_iter()
                .map(|album| {
                    (
                        library::MemberRead::new(library::Collection::Album(album)).selection(),
                        QueueProvenance::Random,
                    )
                })
                .collect(),
        ),
        _ => return Ok(true),
    };
    queue.play_selection(crate::playback::PlayRequest::from_input(
        load,
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
    let mut similar = None;
    for source_key in fallback_sources {
        if candidates.len() == requested {
            break;
        }
        // The seed's similar artists are looked up once, when a fallback first needs them.
        let similar = match &similar {
            Some(similar) => similar,
            None => similar.insert(
                similar_artist_mbids(catalog, &source_owner, seed_source_key, &seed, cancellation)
                    .await,
            ),
        };
        let similar = catalog
            .artists_with_mbids(source_key, similar, cancellation)
            .await
            .map_err(|error| error.to_string())?;
        let fallback = catalog
            .radio_candidates(
                source_key,
                seed.clone(),
                &similar,
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

/// The MusicBrainz ids of the artists ListenBrainz's listeners play alongside the seed's first
/// two artists. None where external lookups are off, the seed's artists carry no ids, or the
/// lookup fails: the Catalog's own relations then pick alone.
async fn similar_artist_mbids(
    catalog: &Catalog,
    source_owner: &SourceOwner,
    seed_source: Option<SourceKey>,
    seed: &RadioSeed,
    cancellation: &ReadCancellation,
) -> Vec<String> {
    let Some(seed_source) = seed_source else {
        return Vec::new();
    };
    if !source_owner
        .shared
        .settings
        .committed()
        .allows_external_metadata_lookup()
    {
        return Vec::new();
    }
    let Ok(mut artists) = catalog
        .radio_artist_mbids(seed_source, seed, cancellation)
        .await
    else {
        return Vec::new();
    };
    artists.truncate(2);
    if artists.is_empty() {
        return Vec::new();
    }
    let cache = metadata_lookup::LookupCache::new(
        catalog.clone(),
        tokio::runtime::Handle::current(),
        metadata_lookup::CacheMode::Use,
    );
    tokio::task::spawn_blocking(move || {
        artists
            .iter()
            .filter_map(|artist| metadata_lookup::similar_artists(&cache, artist).ok())
            .flatten()
            .collect()
    })
    .await
    .unwrap_or_default()
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
