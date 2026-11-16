//! Radio and Auto DJ picks: the server's own where the user wants them, Rufin's otherwise.

use std::sync::Arc;

use library::{Catalog, RadioSeed, ReadCancellation, SourceKey};
use sources::{Source, SourceRadioSeed};

use crate::playback::random_u64;
use crate::runtime::SelectedLibrary;
use crate::source::SourceOwner;

pub(crate) async fn radio_candidates(
    catalog: &Catalog,
    source_owner: Arc<SourceOwner>,
    selected: Option<SelectedLibrary>,
    seed: RadioSeed,
    requested: usize,
    cancellation: &ReadCancellation,
) -> Result<Vec<library::TrackKey>, String> {
    let requested = requested.min(library::QUEUE_CONTEXT_LIMIT);
    let settings = source_owner.shared.settings.committed();
    let taste = library::RadioTaste {
        same_artist: settings.application.auto_dj_same_artist,
        skip_low_rated: settings.application.auto_dj_skip_low_rated,
    };
    let seed_source = catalog
        .radio_source(&seed, cancellation)
        .await
        .map_err(|error| error.to_string())?;
    let mut candidates = Vec::new();
    let seed_source_key = seed_source.as_ref().map(|(key, _, _)| *key);
    let mut fallback_sources = Vec::new();
    if let Some((source_key, source_id, object_id)) = seed_source {
        // The server's own picks play as it orders them; Rufin's fill what it leaves short.
        if !settings.overrides_server_recommendations() {
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
                    .recommended_track_keys(
                        source_key,
                        &seed,
                        &ids,
                        taste.skip_low_rated,
                        cancellation,
                    )
                    .await
                    .map_err(|error| error.to_string())?;
            }
        }
        fallback_sources.push(source_key);
    }
    if let Some(selected) = selected
        && Some(selected.source_key) != seed_source_key
    {
        fallback_sources.push(selected.source_key);
    }
    candidates.truncate(requested);
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_secs() as i64);
    let mut similar = None;
    for source_key in fallback_sources {
        if candidates.len() == requested {
            break;
        }
        // The seed's similar artists are looked up once, when Rufin first picks.
        let similar = match &similar {
            Some(similar) => similar,
            None => similar.insert(
                similar_artists(catalog, &source_owner, seed_source_key, &seed, cancellation).await,
            ),
        };
        let fallback = catalog
            .radio_candidates(
                source_key,
                seed.clone(),
                similar,
                taste,
                &candidates,
                requested - candidates.len(),
                now,
                random_u64() as i64,
                cancellation,
            )
            .await
            .map_err(|error| error.to_string())?;
        candidates.extend(fallback);
    }
    Ok(candidates)
}

/// The artists ListenBrainz's listeners play alongside the seed's first two artists, closest
/// first. An artist whose tags carry no MusicBrainz id is found there by its name. None where
/// external lookups or this one are off, the seed names no artist, or the lookups fail: the
/// library's own relations then pick alone.
async fn similar_artists(
    catalog: &Catalog,
    source_owner: &SourceOwner,
    seed_source: Option<SourceKey>,
    seed: &RadioSeed,
    cancellation: &ReadCancellation,
) -> Vec<library::SimilarArtist> {
    let Some(seed_source) = seed_source else {
        return Vec::new();
    };
    let settings = source_owner.shared.settings.committed();
    if !settings.allows_external_metadata_lookup() || !settings.application.auto_dj_similar_artists
    {
        return Vec::new();
    }
    let Ok(mut artists) = catalog
        .radio_seed_artists(seed_source, seed, cancellation)
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
        let named = |name: &str| {
            metadata_lookup::artist_id_by_name(&cache, name)
                .ok()
                .flatten()
        };
        let seeds = artists
            .into_iter()
            .filter_map(|(name, mbid)| {
                // A credit of several artists goes by the first of them MusicBrainz knows.
                mbid.or_else(|| named(&name)).or_else(|| {
                    let credited = names::credit_artists(&name);
                    (credited.len() > 1)
                        .then(|| credited.iter().take(2).find_map(|artist| named(artist)))
                        .flatten()
                })
            })
            .collect::<Vec<_>>();
        let mut similar = Vec::<library::SimilarArtist>::new();
        for artist in seeds
            .iter()
            .filter_map(|seed| metadata_lookup::similar_artists(&cache, seed).ok())
            .flatten()
        {
            if seeds.contains(&artist.mbid) {
                continue;
            }
            match similar.iter_mut().find(|known| known.mbid == artist.mbid) {
                Some(known) => known.closeness = known.closeness.max(artist.closeness),
                None => similar.push(artist),
            }
        }
        similar.sort_by(|left, right| right.closeness.total_cmp(&left.closeness));
        similar
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
